//! F59 拖出的 **Windows 实现**:虚拟文件 `IDataObject` + 专用 STA 线程。
//!
//! 三条硬约束决定了这个文件为什么长这样:
//!
//! - **D10 —— 不能在 winit 的回调栈里起 `DoDragDrop`。**`DoDragDrop` 自己跑一个
//!   嵌套模态消息循环,循环里会派发 `WM_PAINT`;而 winit 0.30 的 Windows 后端对
//!   `RedrawRequested` 是**绕过事件缓冲直接回调**的
//!   (`platform_impl/windows/event_loop/runner.rs`:`call_event_handler` 里
//!   `event_handler.take().expect("either event handler is re-entrant (likely)…")`)。
//!   于是嵌套循环里的第一次重绘就把 handler 取空 → panic。所以拖出**必须**在
//!   一条自己的线程上跑,UI 线程只负责 spawn。
//! - **D11 —— 虚拟文件,不预下载。**给 `CFSTR_FILEDESCRIPTORW`(名字+大小)+
//!   `CFSTR_FILECONTENTS`(按 `lindex` 给 `IStream`),目标程序读流的时候我们才
//!   真去 SFTP 拉。否则「拖之前先把 2GB 下完」,手势会僵在那儿。
//! - **跨线程调用。**流是目标程序(资源管理器,**另一个进程**)读的,调用落在
//!   哪条线程不由我们决定。`windows-implement` 生成的 `QueryInterface` 已经应答
//!   `IAgileObject`,对象因此被 COM 当作敏捷对象、可跨套间直接编组 —— 这正是
//!   设计里 `CoCreateFreeThreadedMarshaler` 想要的效果,而后者需要 COM 聚合,
//!   `#[implement]` 给不出。**内部状态一律 `Mutex` 包起来**,不能假设单线程。
//!
//! 另有一条不写代码看不出来的:**COM 回调是 FFI 边界,panic 不许穿过去**。
//! `block_on` 里 SFTP 那一侧任何 panic 都会变成未定义行为(而现象是「拖一下
//! 整个程序没了」)。所有会 `block_on` 的入口都套 `catch_unwind`。
//!
//! 这一整个文件在无头容器里**一行都验不了**,只有 Windows 实机能验。能自动
//! 保证的只有「交叉编译过得去」,以及 D12 的日志够不够诊断 —— 失败发生在
//! 别人的进程里,日志是唯一的抓手。

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use windows::core::{implement, Ref, Result as WinResult, HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    GlobalFree, BOOL, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
    DV_E_FORMATETC, DV_E_LINDEX, DV_E_TYMED, E_FAIL, E_NOTIMPL, E_OUTOFMEMORY, HGLOBAL,
    OLE_E_ADVISENOTSUPPORTED, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{
    CoTaskMemAlloc, IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC,
    IEnumFORMATETC_Impl, IEnumSTATDATA, ISequentialStream_Impl, IStream, IStream_Impl, DATADIR_GET,
    DVASPECT_CONTENT, FORMATETC, LOCKTYPE, STATFLAG, STATFLAG_NONAME, STATSTG, STGC, STGMEDIUM,
    STGMEDIUM_0, STGM_READ, STGTY_STREAM, STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_SET,
    TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize, DROPEFFECT,
    DROPEFFECT_COPY,
};
use windows::Win32::System::SystemServices::MK_LBUTTON;

use mullion_ssh::sftp::{RemoteFile, RemotePath, SftpClient};

use super::{descriptor, DragOutItem, LOG};

/// 两个剪贴板格式的 ID。`RegisterClipboardFormatW` 是幂等的(同名返回同一个 ID),
/// 但它要过一次系统调用,而 `GetData` 在一次拖里会被问很多遍 —— 起拖时算一次。
#[derive(Clone, Copy)]
struct Formats {
    descriptor: u16,
    contents: u16,
}

impl Formats {
    fn register() -> Self {
        // SAFETY:两个字面量都是以 NUL 结尾的 UTF-16 常量。
        unsafe {
            Self {
                descriptor: RegisterClipboardFormatW(w("FileGroupDescriptorW").as_pcwstr()) as u16,
                contents: RegisterClipboardFormatW(w("FileContents").as_pcwstr()) as u16,
            }
        }
    }
}

/// 一个带所有权的 UTF-16 NUL 结尾串。`PCWSTR` 只是裸指针,直接对临时 `Vec`
/// 取指针会当场悬垂 —— 这类错误编译期不报,运行期是随机的乱码格式名。
struct W(Vec<u16>);

impl W {
    fn as_pcwstr(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr())
    }
}

fn w(s: &str) -> W {
    W(s.encode_utf16().chain(std::iter::once(0)).collect())
}

// ---------------------------------------------------------------- IStream

struct StreamState {
    /// 远端句柄。**懒开** —— 起拖时目标程序还没决定要不要读,提前开一堆
    /// 句柄的话「拖过去又拖回来」会白占远端的 fd。
    file: Option<RemoteFile>,
    pos: u64,
}

/// 一个远端文件的只读流。目标程序读一块,我们才去 SFTP 拉一块。
#[implement(IStream)]
struct SftpStream {
    runtime: tokio::runtime::Handle,
    sftp: Arc<SftpClient>,
    path: RemotePath,
    size: u64,
    name: Vec<u16>,
    state: Mutex<StreamState>,
}

impl SftpStream {
    fn new(runtime: tokio::runtime::Handle, sftp: Arc<SftpClient>, item: &DragOutItem) -> Self {
        Self {
            runtime,
            sftp,
            path: item.remote.clone(),
            size: item.size,
            name: w(&item.name).0,
            state: Mutex::new(StreamState { file: None, pos: 0 }),
        }
    }

    /// 往 `out` 里灌一块。返回真正读到的字节数(0 = 到底了)。
    fn fill(&self, out: &mut [u8]) -> Result<usize, HRESULT> {
        // 中毒的锁照样用:上一次调用 panic 过不代表这条流没救了,而返回
        // 一个错误码只会让目标程序显示「拖过去没反应」。
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.file.is_none() {
            let opened = self.runtime.block_on(self.sftp.open_read(&self.path));
            match opened {
                Ok(f) => st.file = Some(f),
                Err(e) => {
                    log::warn!(target: LOG, "打开远端文件失败 {}: {e}", self.path.display());
                    return Err(E_FAIL);
                }
            }
        }
        let file = st.file.as_mut().expect("刚刚才置上");
        match self.runtime.block_on(file.read_chunk(out)) {
            Ok(n) => {
                st.pos += n as u64;
                Ok(n)
            }
            Err(e) => {
                log::warn!(target: LOG, "读远端文件失败 {}: {e}", self.path.display());
                Err(E_FAIL)
            }
        }
    }

    /// 回到开头 = **重开一次**。SFTP 这一层只给顺序读(`read_chunk`),
    /// 没有 seek;重开比自己维护偏移简单,而且只在目标程序真要重读时才发生。
    fn rewind(&self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.file = None;
        st.pos = 0;
    }

    fn pos(&self) -> u64 {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).pos
    }
}

impl ISequentialStream_Impl for SftpStream_Impl {
    fn Read(&self, pv: *mut c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        if !pcbread.is_null() {
            // 先清零:下面任何一条错误路径都不该让调用方读到栈上的垃圾。
            unsafe { *pcbread = 0 };
        }
        if pv.is_null() || cb == 0 {
            return S_OK;
        }
        // COM 回调是 FFI 边界,panic 穿过去是未定义行为,而现象是「拖一下
        // 整个程序没了」。SFTP 那一侧(以及 tokio 的 block_on)都可能 panic。
        let out = unsafe { std::slice::from_raw_parts_mut(pv as *mut u8, cb as usize) };
        let done = catch_unwind(AssertUnwindSafe(|| self.fill(out)));
        let n = match done {
            Ok(Ok(n)) => n,
            Ok(Err(hr)) => return hr,
            Err(_) => {
                log::error!(target: LOG, "读流时 panic,已挡在 COM 边界内");
                return E_FAIL;
            }
        };
        if !pcbread.is_null() {
            unsafe { *pcbread = n as u32 };
        }
        S_OK
    }

    fn Write(&self, _pv: *const c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        // 拖出是只读方向。
        E_NOTIMPL
    }
}

impl IStream_Impl for SftpStream_Impl {
    fn Seek(
        &self,
        dlibmove: i64,
        dworigin: STREAM_SEEK,
        plibnewposition: *mut u64,
    ) -> WinResult<()> {
        // 只认两种:回到开头(重读)、原地问位置。SFTP 这层没有随机读,
        // 装作支持的话目标程序会拿到错位的内容 —— 那比明着报错糟得多。
        let pos = if dworigin == STREAM_SEEK_SET && dlibmove == 0 {
            self.rewind();
            0
        } else if dworigin == STREAM_SEEK_CUR && dlibmove == 0 {
            self.pos()
        } else {
            log::debug!(
                target: LOG,
                "目标程序要随机 seek(move={dlibmove}, origin={}),不支持",
                dworigin.0
            );
            return Err(E_NOTIMPL.into());
        };
        if !plibnewposition.is_null() {
            unsafe { *plibnewposition = pos };
        }
        Ok(())
    }

    fn Stat(&self, pstatstg: *mut STATSTG, grfstatflag: &STATFLAG) -> WinResult<()> {
        if pstatstg.is_null() {
            return Err(E_FAIL.into());
        }
        let mut st = STATSTG {
            r#type: STGTY_STREAM.0 as u32,
            cbSize: self.size,
            grfMode: STGM_READ,
            ..Default::default()
        };
        if *grfstatflag != STATFLAG_NONAME {
            // 名字要用 `CoTaskMemFree` 能释放的内存 —— 调用方负责释放,
            // 给 Rust 的堆指针会在对方进程里炸。
            let bytes = self.name.len() * 2;
            let p = unsafe { CoTaskMemAlloc(bytes) } as *mut u16;
            if p.is_null() {
                return Err(E_OUTOFMEMORY.into());
            }
            unsafe { std::ptr::copy_nonoverlapping(self.name.as_ptr(), p, self.name.len()) };
            st.pwcsName = windows::core::PWSTR(p);
        }
        unsafe { *pstatstg = st };
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> WinResult<()> {
        Err(E_NOTIMPL.into())
    }

    fn CopyTo(
        &self,
        _pstm: Ref<'_, IStream>,
        _cb: u64,
        _pcbread: *mut u64,
        _pcbwritten: *mut u64,
    ) -> WinResult<()> {
        // 目标程序自己拿 Read 循环。实现它要再写一遍分块逻辑,没有收益。
        Err(E_NOTIMPL.into())
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> WinResult<()> {
        Err(E_NOTIMPL.into())
    }

    fn Revert(&self) -> WinResult<()> {
        Err(E_NOTIMPL.into())
    }

    fn LockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: &LOCKTYPE) -> WinResult<()> {
        Err(E_NOTIMPL.into())
    }

    fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> WinResult<()> {
        Err(E_NOTIMPL.into())
    }

    fn Clone(&self) -> WinResult<IStream> {
        // 克隆要能独立 seek,而我们连 seek 都没有。
        Err(E_NOTIMPL.into())
    }
}

// ------------------------------------------------------------ IDataObject

/// 这一拖的全部内容。目标程序问什么给什么。
#[implement(IDataObject)]
struct Items {
    formats: Formats,
    /// `CFSTR_FILEDESCRIPTORW` 的字节,起拖时就算好(名字和大小都已知)。
    descriptor: Vec<u8>,
    items: Vec<DragOutItem>,
    runtime: tokio::runtime::Handle,
    sftp: Arc<SftpClient>,
}

impl Items {
    /// 这个 `FORMATETC` 我们认不认。认的话返回它是第几种。
    fn accepts(&self, fe: &FORMATETC) -> Result<Wanted, HRESULT> {
        if fe.dwAspect != DVASPECT_CONTENT.0 {
            return Err(DV_E_FORMATETC);
        }
        if fe.cfFormat == self.formats.descriptor {
            if fe.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
                return Err(DV_E_TYMED);
            }
            return Ok(Wanted::Descriptor);
        }
        if fe.cfFormat == self.formats.contents {
            if fe.tymed & TYMED_ISTREAM.0 as u32 == 0 {
                return Err(DV_E_TYMED);
            }
            let i = fe.lindex;
            if i < 0 || i as usize >= self.items.len() {
                return Err(DV_E_LINDEX);
            }
            return Ok(Wanted::Contents(i as usize));
        }
        Err(DV_E_FORMATETC)
    }
}

enum Wanted {
    Descriptor,
    Contents(usize),
}

/// 把一段字节搬进 `GMEM_MOVEABLE` 的全局内存 —— `TYMED_HGLOBAL` 要的就是它,
/// 而且**所有权交给调用方**(由 `ReleaseStgMedium` 释放)。
fn to_hglobal(bytes: &[u8]) -> Result<HGLOBAL, HRESULT> {
    let h = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }.map_err(|_| E_OUTOFMEMORY)?;
    let p = unsafe { GlobalLock(h) };
    if p.is_null() {
        let _ = unsafe { GlobalFree(Some(h)) };
        return Err(E_OUTOFMEMORY);
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len()) };
    // 锁计数归零时 `GlobalUnlock` 返回 FALSE 且 `GetLastError()` 是
    // `NO_ERROR` —— 这是成功,不是失败。判它的返回值反而会误报。
    let _ = unsafe { GlobalUnlock(h) };
    Ok(h)
}

impl IDataObject_Impl for Items_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> WinResult<STGMEDIUM> {
        if pformatetcin.is_null() {
            return Err(E_FAIL.into());
        }
        let fe = unsafe { *pformatetcin };
        match self.accepts(&fe) {
            Ok(Wanted::Descriptor) => {
                log::debug!(target: LOG, "目标程序取描述符({} 项)", self.items.len());
                let h = to_hglobal(&self.descriptor).map_err(windows::core::Error::from)?;
                Ok(STGMEDIUM {
                    tymed: TYMED_HGLOBAL.0 as u32,
                    u: STGMEDIUM_0 { hGlobal: h },
                    pUnkForRelease: std::mem::ManuallyDrop::new(None),
                })
            }
            Ok(Wanted::Contents(i)) => {
                let item = &self.items[i];
                log::debug!(target: LOG, "目标程序取第 {i} 项的流:{}", item.remote.display());
                let stream: IStream =
                    SftpStream::new(self.runtime.clone(), self.sftp.clone(), item).into();
                Ok(STGMEDIUM {
                    tymed: TYMED_ISTREAM.0 as u32,
                    u: STGMEDIUM_0 {
                        pstm: std::mem::ManuallyDrop::new(Some(stream)),
                    },
                    pUnkForRelease: std::mem::ManuallyDrop::new(None),
                })
            }
            Err(hr) => {
                // D12:目标程序要了个我们不给的格式。「拖过去没反应」十有八九
                // 停在这一行 —— 比如只认 `CF_HDROP` 的老程序。
                log::debug!(
                    target: LOG,
                    "拒绝格式 cf={} tymed={} lindex={}:{hr:?}",
                    fe.cfFormat, fe.tymed, fe.lindex
                );
                Err(hr.into())
            }
        }
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> WinResult<()> {
        // 「往调用方给的缓冲里写」。`TYMED_ISTREAM` 用不到这条。
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        if pformatetc.is_null() {
            return E_FAIL;
        }
        let fe = unsafe { *pformatetc };
        match self.accepts(&fe) {
            Ok(_) => S_OK,
            Err(hr) => hr,
        }
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatectin: *const FORMATETC,
        pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        // 即使返回 E_NOTIMPL,规范也要求把 `ptd` 置空 —— 不置的话调用方可能
        // 去释放一个没初始化的指针。
        if !pformatetcout.is_null() {
            unsafe { *pformatetcout = FORMATETC::default() };
        }
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: BOOL,
    ) -> WinResult<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> WinResult<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            // 写方向没有任何格式。
            return Err(E_NOTIMPL.into());
        }
        Ok(FormatEnum::new(self.formats, self.items.len()).into())
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: Ref<'_, IAdviseSink>,
    ) -> WinResult<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _dwconnection: u32) -> WinResult<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> WinResult<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

// --------------------------------------------------------- IEnumFORMATETC

/// 我们提供的格式清单:一条描述符 + 每项一条内容。
///
/// 资源管理器**先枚举再取数**,这里少列一条,`GetData` 那边给得再对也没人来问。
#[implement(IEnumFORMATETC)]
struct FormatEnum {
    all: Vec<FORMATETC>,
    at: Mutex<usize>,
}

impl FormatEnum {
    fn new(formats: Formats, n: usize) -> Self {
        let mut all = vec![FORMATETC {
            cfFormat: formats.descriptor,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        }];
        all.extend((0..n).map(|i| FORMATETC {
            cfFormat: formats.contents,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: i as i32,
            tymed: TYMED_ISTREAM.0 as u32,
        }));
        Self {
            all,
            at: Mutex::new(0),
        }
    }
}

impl IEnumFORMATETC_Impl for FormatEnum_Impl {
    fn Next(&self, celt: u32, rgelt: *mut FORMATETC, pceltfetched: *mut u32) -> HRESULT {
        let mut at = self.at.lock().unwrap_or_else(|e| e.into_inner());
        let n = (self.all.len().saturating_sub(*at)).min(celt as usize);
        if !rgelt.is_null() {
            for k in 0..n {
                unsafe { *rgelt.add(k) = self.all[*at + k] };
            }
        }
        *at += n;
        if !pceltfetched.is_null() {
            unsafe { *pceltfetched = n as u32 };
        }
        // 要够数才是 S_OK。给少了还报 S_OK,调用方会去读没写过的那几格。
        if n == celt as usize {
            S_OK
        } else {
            S_FALSE
        }
    }

    fn Skip(&self, celt: u32) -> WinResult<()> {
        let mut at = self.at.lock().unwrap_or_else(|e| e.into_inner());
        *at = (*at + celt as usize).min(self.all.len());
        Ok(())
    }

    fn Reset(&self) -> WinResult<()> {
        *self.at.lock().unwrap_or_else(|e| e.into_inner()) = 0;
        Ok(())
    }

    fn Clone(&self) -> WinResult<IEnumFORMATETC> {
        let at = *self.at.lock().unwrap_or_else(|e| e.into_inner());
        let dup = FormatEnum {
            all: self.all.clone(),
            at: Mutex::new(at),
        };
        Ok(dup.into())
    }
}

// ------------------------------------------------------------- IDropSource

#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(
        &self,
        fescapepressed: BOOL,
        grfkeystate: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
    ) -> HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        // 左键松开 = 放手。注意**不能**反过来判「右键按着」——
        // 用右键拖是「拖放到目标后弹菜单」的合法手势。
        if grfkeystate.0 & MK_LBUTTON.0 == 0 {
            return DRAGDROP_S_DROP;
        }
        S_OK
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        // 交给系统画默认光标。自绘光标是另一件事(不在 F59 范围里)。
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

// ------------------------------------------------------------------ 入口

/// 起一条拖出。**立刻返回**,真正的 `DoDragDrop` 在新线程里跑(D10)。
///
/// 线程结束时负责 [`super::finished`] —— 不然重入闸门会永久卡死,
/// 用户「拖过一次之后再也拖不动了」。
pub fn start(runtime: tokio::runtime::Handle, sftp: Arc<SftpClient>, items: Vec<DragOutItem>) {
    let spawned = std::thread::Builder::new()
        .name("mullion-dragout".into())
        .spawn(move || {
            let _guard = Finish;
            run(runtime, sftp, items);
        });
    if let Err(e) = spawned {
        log::error!(target: LOG, "起拖出线程失败:{e}");
        super::finished();
    }
}

/// 线程无论怎么退(正常/panic)都要放闸。
struct Finish;

impl Drop for Finish {
    fn drop(&mut self) {
        super::finished();
    }
}

fn run(runtime: tokio::runtime::Handle, sftp: Arc<SftpClient>, items: Vec<DragOutItem>) {
    // 这条线程自己是 STA:`DoDragDrop` 要求调用线程初始化过 OLE。
    if let Err(e) = unsafe { OleInitialize(None) } {
        log::error!(target: LOG, "OleInitialize 失败:{e}");
        return;
    }
    let started = Instant::now();
    let formats = Formats::register();
    let described: Vec<descriptor::Described<'_>> = items
        .iter()
        .map(|i| descriptor::Described {
            name: &i.name,
            size: i.size,
        })
        .collect();
    let bytes = descriptor::file_group_descriptor(&described);
    log::info!(
        target: LOG,
        "拖出开始:{} 项,描述符 {} 字节,格式 id 描述符={} 内容={}",
        items.len(), bytes.len(), formats.descriptor, formats.contents
    );

    let data: IDataObject = Items {
        formats,
        descriptor: bytes,
        items,
        runtime,
        sftp,
    }
    .into();
    let source: IDropSource = DropSource.into();

    let mut effect = DROPEFFECT::default();
    let hr = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
    // D12:这一行是判断「到底放没放下」的唯一依据 —— 真正的落地发生在
    // 目标程序里,我们看不见。
    log::info!(
        target: LOG,
        "拖出结束:hr={hr:?} effect={} 耗时 {} ms",
        effect.0,
        started.elapsed().as_millis()
    );
    unsafe { OleUninitialize() };
}
