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
    /// F230:Mullion 私有格式的 id。`0` = 这一份不带私有载荷(拖出走的就是
    /// 这一支 —— 拖到别的 Mullion 不在 F230 范围里)。
    private_format: u16,
    /// F230:私有格式的字节(`clipboard_remote::wrap` 过的信封)。
    private_payload: Vec<u8>,
}

impl Items {
    /// 这个 `FORMATETC` 我们认不认。认的话返回它是第几种。
    fn accepts(&self, fe: &FORMATETC) -> Result<Wanted, HRESULT> {
        if fe.dwAspect != DVASPECT_CONTENT.0 {
            return Err(DV_E_FORMATETC);
        }
        // F230:私有格式排在最前面。`private_format == 0` 是「这一份没有私有
        // 载荷」的哨兵 —— 不判它的话,一个 `cfFormat == 0` 的探询会被当成
        // 私有格式应答,给回去一个空 HGLOBAL。
        if self.private_format != 0 && fe.cfFormat == self.private_format {
            if fe.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
                return Err(DV_E_TYMED);
            }
            return Ok(Wanted::Private);
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
    /// F230:Mullion 私有格式(远端路径 + 源端主机指纹)。
    Private,
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
            Ok(Wanted::Private) => {
                log::debug!(target: LOG, "对面取私有格式({} 字节)", self.private_payload.len());
                let h = to_hglobal(&self.private_payload).map_err(windows::core::Error::from)?;
                Ok(STGMEDIUM {
                    tymed: TYMED_HGLOBAL.0 as u32,
                    u: STGMEDIUM_0 { hGlobal: h },
                    pUnkForRelease: std::mem::ManuallyDrop::new(None),
                })
            }
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
        Ok(FormatEnum::new(self.formats, self.items.len(), self.private_format).into())
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
    fn new(formats: Formats, n: usize, private: u16) -> Self {
        let mut all = Vec::new();
        // F230:私有格式也要**列**出来。只在 `GetData`/`QueryGetData` 里认它,
        // 而枚举里不列的话,先枚举再取数的对面根本不知道有这个格式 ——
        // 粘不出来且完全静默(本仓库管这叫「列举式门控在加档时必然漏」)。
        if private != 0 {
            all.push(FORMATETC {
                cfFormat: private,
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            });
        }
        all.push(FORMATETC {
            cfFormat: formats.descriptor,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        });
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
            // 拖出仍然跳过目录(设计 N2):起拖那一刻不能卡几十秒去递归列目录。
            is_dir: false,
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
        // 拖出不带私有载荷:F230 只做「复制粘贴」跨实例,拖拽跨实例是另一件事。
        private_format: 0,
        private_payload: Vec::new(),
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

// ------------------------------------------- F230:系统剪贴板(常驻 STA 线程)

/// 从系统剪贴板读回来的东西。两种格式**一次取完** —— 每问一次
/// `OleGetClipboard` 都是一次跨进程往返,而 Ctrl+V 是同步手势。
#[derive(Debug, Default)]
pub struct ClipRead {
    /// Mullion 私有格式的正文(信封已拆)。`None` = 板上没有我们认识的东西。
    pub private: Option<Vec<u8>>,
    /// `CF_HDROP` —— 用户在资源管理器里 Ctrl+C 的本地文件。
    pub hdrop: Vec<std::path::PathBuf>,
}

/// 发给常驻 STA 线程的活。
enum ClipJob {
    /// 把这一批远端文件放进系统剪贴板。
    Set {
        payload: Vec<u8>,
        items: Vec<DragOutItem>,
        runtime: tokio::runtime::Handle,
        sftp: Arc<SftpClient>,
    },
    /// 读一次系统剪贴板,结果回传。
    Read(std::sync::mpsc::Sender<ClipRead>),
    /// F230 慢路径:把板上的虚拟文件内容拉到 `into` 目录下。
    ///
    /// 回调而不是事件:`dragout` 不认识 `UserEvent`(它是 app 的类型),
    /// 让它认识就等于把 UI 的类型漏进这一层。
    Fetch {
        into: std::path::PathBuf,
        #[allow(clippy::type_complexity)]
        done: Box<dyn FnOnce(Result<Vec<std::path::PathBuf>, String>) + Send>,
    },
}

/// 常驻 STA 线程的投递口。**必须常驻**:`OleSetClipboard` 之后,延迟渲染的
/// `GetData` 回调会回到**放剪贴板的那条线程**。F59 那种一次性线程在
/// `DoDragDrop` 返回后就退出了 —— 用它放剪贴板,用户按 Ctrl+V 时没人接
/// 回调,粘出来是空的,而且完全静默(我们这边一条日志都不会有,失败发生
/// 在别人的进程里)。
static CLIP_TX: std::sync::OnceLock<std::sync::mpsc::Sender<ClipJob>> = std::sync::OnceLock::new();

/// 取(必要时启动)常驻 STA 线程的投递口。
fn clip_tx() -> &'static std::sync::mpsc::Sender<ClipJob> {
    CLIP_TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<ClipJob>();
        let spawned = std::thread::Builder::new()
            .name("mullion-clipboard".into())
            .spawn(move || clip_thread(&rx));
        if let Err(e) = spawned {
            log::error!(target: LOG, "起剪贴板 STA 线程失败:{e}");
        }
        tx
    })
}

fn clip_thread(rx: &std::sync::mpsc::Receiver<ClipJob>) {
    if let Err(e) = unsafe { OleInitialize(None) } {
        log::error!(target: LOG, "剪贴板线程 OleInitialize 失败:{e}");
        return;
    }
    // **不 `OleUninitialize`**:这条线程活到进程结束。提前 uninit 会让已经
    // 放进剪贴板的 `IDataObject` 失效,用户按 Ctrl+V 粘出来是空的。
    loop {
        // 等活,但要在等的间隙抽消息泵 —— STA 线程不抽泵,跨进程的
        // `GetData` 编组会死等(而延迟渲染的回调正是这么调进来的)。
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(ClipJob::Set {
                payload,
                items,
                runtime,
                sftp,
            }) => set_clipboard(payload, items, runtime, sftp),
            Ok(ClipJob::Read(reply)) => {
                // 读失败也要回一份空的:调用方在等这条回音,不回它就一直
                // 卡到超时,Ctrl+V 变成「按下去愣半秒才有反应」。
                let _ = reply.send(read_clipboard());
            }
            Ok(ClipJob::Fetch { into, done }) => done(fetch_virtual_files(&into)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
        pump_messages();
    }
}

/// 抽干这条线程当前排队的窗口消息。
fn pump_messages() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    let mut msg = MSG::default();
    while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// 私有格式的 id。`RegisterClipboardFormatW` 幂等(同名同 id),这正是
/// 「两个 Mullion 互通」要的 —— 两个进程各自登记同一个名字拿到同一个号。
fn private_format_id() -> u16 {
    unsafe { RegisterClipboardFormatW(w(crate::clipboard_remote::FORMAT_NAME).as_pcwstr()) as u16 }
}

fn set_clipboard(
    payload: Vec<u8>,
    items: Vec<DragOutItem>,
    runtime: tokio::runtime::Handle,
    sftp: Arc<SftpClient>,
) {
    use windows::Win32::System::Ole::OleSetClipboard;
    let formats = Formats::register();
    let private_format = private_format_id();
    let described: Vec<descriptor::Described<'_>> = items
        .iter()
        .map(|i| descriptor::Described {
            name: &i.name,
            size: i.size,
            // 虚拟文件这一侧只放普通文件:目录展开要在 Ctrl+C 那一刻递归列
            // 远端目录(可能几十秒)。粘到别的 Mullion 里走私有格式,那条
            // 路上目录是远端自己 `copy_tree`,不需要展开。
            is_dir: false,
        })
        .collect();
    let descriptor_bytes = descriptor::file_group_descriptor(&described);
    log::info!(
        target: LOG,
        "放剪贴板:{} 个虚拟文件,私有载荷 {} 字节,格式 id 私有={} 描述符={}",
        items.len(), payload.len(), private_format, formats.descriptor
    );
    let data: IDataObject = Items {
        formats,
        descriptor: descriptor_bytes,
        items,
        runtime,
        sftp,
        private_format,
        private_payload: crate::clipboard_remote::wrap(&payload),
    }
    .into();
    match unsafe { OleSetClipboard(&data) } {
        Ok(()) => log::info!(target: LOG, "已放入系统剪贴板"),
        Err(e) => log::error!(target: LOG, "OleSetClipboard 失败:{e}"),
    }
    // **不调 `OleFlushClipboard`**:那会把所有延迟渲染的内容立刻拉下来 ——
    // 一棵几百 MB 的远端目录树会当场下载完。代价是「Mullion 一关剪贴板就
    // 失效」,这是与用户确认过、明确接受的取舍(F230)。
}

/// 一个 `TYMED_HGLOBAL` 的 `FORMATETC`。
fn hglobal_formatetc(cf: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// 取一个 `TYMED_HGLOBAL` 格式的原始字节。**长度按 `GlobalSize` 量** ——
/// 它允许比对面申请的大,所以尾部可能有填充,拆信封那一步负责裁掉
/// (见 `clipboard_remote::wrap`)。
fn hglobal_bytes(data: &IDataObject, cf: u16) -> Option<Vec<u8>> {
    use windows::Win32::System::Ole::ReleaseStgMedium;
    let fe = hglobal_formatetc(cf);
    // 先问一句:对面不认这个格式时 `GetData` 也会失败,但 `QueryGetData`
    // 便宜得多,而 Ctrl+V 每按一次都要问两种格式。
    if unsafe { data.QueryGetData(&fe) } != S_OK {
        return None;
    }
    let mut medium = unsafe { data.GetData(&fe) }.ok()?;
    let out = read_medium(&medium);
    // `GetData` 拿回来的 medium 归**我们**释放,漏了就是每按一次 Ctrl+V
    // 泄一块全局内存,而且不会有任何报错。
    unsafe { ReleaseStgMedium(&mut medium) };
    return out;

    fn read_medium(medium: &STGMEDIUM) -> Option<Vec<u8>> {
        use windows::Win32::System::Memory::GlobalSize;
        if medium.tymed != TYMED_HGLOBAL.0 as u32 {
            return None;
        }
        let h = unsafe { medium.u.hGlobal };
        let size = unsafe { GlobalSize(h) };
        if size == 0 {
            return None;
        }
        let p = unsafe { GlobalLock(h) };
        if p.is_null() {
            return None;
        }
        let v = unsafe { std::slice::from_raw_parts(p as *const u8, size) }.to_vec();
        let _ = unsafe { GlobalUnlock(h) };
        Some(v)
    }
}

/// 解 `CF_HDROP`:资源管理器里 Ctrl+C 的本地文件清单。
fn hdrop_paths(data: &IDataObject) -> Vec<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::Ole::{ReleaseStgMedium, CF_HDROP};
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    let fe = hglobal_formatetc(CF_HDROP.0);
    if unsafe { data.QueryGetData(&fe) } != S_OK {
        return Vec::new();
    }
    let Ok(mut medium) = (unsafe { data.GetData(&fe) }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if medium.tymed == TYMED_HGLOBAL.0 as u32 {
        let hdrop = HDROP(unsafe { medium.u.hGlobal }.0);
        // `0xFFFF_FFFF` 是「告诉我一共几个」的约定值,不是第 42 亿项。
        let n = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
        for i in 0..n {
            let len = unsafe { DragQueryFileW(hdrop, i, None) } as usize;
            // `DragQueryFileW` 报的长度**不含**结尾 NUL,而带缓冲那一路要
            // 有位置放它 —— 少给一格的话名字会被截掉最后一个字符。
            let mut buf = vec![0u16; len + 1];
            let got = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) } as usize;
            buf.truncate(got);
            out.push(std::path::PathBuf::from(std::ffi::OsString::from_wide(
                &buf,
            )));
        }
    }
    // **不调 `DragFinish`**:这个 `HDROP` 是 medium 的一部分,归
    // `ReleaseStgMedium` 释放。两个都调就是双重释放。
    unsafe { ReleaseStgMedium(&mut medium) };
    out
}

fn read_clipboard() -> ClipRead {
    use windows::Win32::System::Ole::OleGetClipboard;
    // 用 `OleGetClipboard` 而不是 `OpenClipboard`+`GetClipboardData`:
    // 后者只看得见「已经落板」的数据,而 OLE 放进去的是延迟渲染的对象 ——
    // 必须走 OLE 这条路才会回调到源进程去要内容。
    let data = match unsafe { OleGetClipboard() } {
        Ok(d) => d,
        Err(e) => {
            log::debug!(target: LOG, "读剪贴板失败:{e}");
            return ClipRead::default();
        }
    };
    let fmt = private_format_id();
    let private = (fmt != 0)
        .then(|| hglobal_bytes(&data, fmt))
        .flatten()
        .and_then(|b| crate::clipboard_remote::unwrap_blob(&b).map(<[u8]>::to_vec));
    let hdrop = hdrop_paths(&data);
    log::debug!(
        target: LOG,
        "读剪贴板:私有={} 字节,CF_HDROP={} 项",
        private.as_ref().map_or(0, Vec::len), hdrop.len()
    );
    ClipRead { private, hdrop }
}

/// F230 慢路径:把板上登记的**虚拟文件**内容拉到 `into` 目录里,返回落地的
/// 本地路径。
///
/// 为什么必须走这条路:源端在另一台机器上,本进程根本没有到它的连接 ——
/// 唯一拿得到字节的途径就是让**源进程**按延迟渲染的约定把内容吐出来
/// (`CFSTR_FILECONTENTS` 的 `IStream`,它那边自己去 SFTP 拉)。
///
/// 目录不取:虚拟文件那一侧只登记普通文件(展开目录要在 Ctrl+C 那一刻递归
/// 列远端目录)。调用方负责把「跳过了几个目录」说出来。
fn fetch_virtual_files(into: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    use windows::Win32::System::Ole::OleGetClipboard;
    let data = unsafe { OleGetClipboard() }.map_err(|e| format!("读剪贴板失败:{e}"))?;
    let formats = Formats::register();
    let bytes = hglobal_bytes(&data, formats.descriptor)
        .ok_or_else(|| "剪贴板上没有可取的文件内容".to_string())?;
    let listed = descriptor::parse(&bytes);
    std::fs::create_dir_all(into).map_err(|e| format!("建临时目录失败:{e}"))?;
    let mut out = Vec::new();
    for (i, (name, is_dir)) in listed.iter().enumerate() {
        if *is_dir {
            continue;
        }
        // 名字里的反斜杠是相对路径(见 `descriptor::parse`),落地时要连
        // 父目录一起建出来。
        let path = into.join(name.replace('\\', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建临时目录失败:{e}"))?;
        }
        pull_one(&data, formats.contents, i as i32, &path)
            .map_err(|e| format!("取「{name}」失败:{e}"))?;
        out.push(path);
    }
    if out.is_empty() {
        return Err("剪贴板上没有可取的文件内容".into());
    }
    log::info!(target: LOG, "慢路径:已从源端取回 {} 项到 {}", out.len(), into.display());
    Ok(out)
}

/// 把第 `lindex` 项的 `IStream` 抽干写进 `path`。
fn pull_one(
    data: &IDataObject,
    contents: u16,
    lindex: i32,
    path: &std::path::Path,
) -> Result<(), String> {
    use std::io::Write;
    use windows::Win32::System::Ole::ReleaseStgMedium;
    let fe = FORMATETC {
        cfFormat: contents,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex,
        tymed: TYMED_ISTREAM.0 as u32,
    };
    let mut medium = unsafe { data.GetData(&fe) }.map_err(|e| e.to_string())?;
    let result = (|| {
        if medium.tymed != TYMED_ISTREAM.0 as u32 {
            return Err("源端给的不是流".to_string());
        }
        let stream = unsafe { &*medium.u.pstm }
            .clone()
            .ok_or_else(|| "源端给了一个空流".to_string())?;
        let mut f = std::fs::File::create(path).map_err(|e| e.to_string())?;
        // 64 KiB 一块:再大也只是多占内存,`IStream::Read` 那边一次往返的
        // 收益早就平了。
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut got = 0u32;
            unsafe { stream.Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut got)) }
                .ok()
                .map_err(|e| e.to_string())?;
            if got == 0 {
                break;
            }
            f.write_all(&buf[..got as usize])
                .map_err(|e| e.to_string())?;
        }
        // `File::flush` 在 std 里是 no-op,真正的收口是 Drop 时的 close ——
        // 但错误在 Drop 里会被吞掉,所以显式 sync 一次把它捞出来。
        f.sync_all().map_err(|e| e.to_string())
    })();
    unsafe { ReleaseStgMedium(&mut medium) };
    result
}

/// F230 慢路径的入口。**立刻返回** —— 拉内容要走源进程的 SFTP,可能很久,
/// 所以整段在常驻 STA 线程上跑,完了回调。
pub fn fetch_into(
    into: std::path::PathBuf,
    done: impl FnOnce(Result<Vec<std::path::PathBuf>, String>) + Send + 'static,
) {
    if let Err(e) = clip_tx().send(ClipJob::Fetch {
        into,
        done: Box::new(done),
    }) {
        log::error!(target: LOG, "投递取虚拟文件任务失败:{e}");
    }
}

/// 把一批远端文件放进系统剪贴板。**立刻返回** —— 真正的活在常驻 STA 线程里。
pub fn set(
    runtime: tokio::runtime::Handle,
    sftp: Arc<SftpClient>,
    items: Vec<DragOutItem>,
    payload: Vec<u8>,
) {
    if let Err(e) = clip_tx().send(ClipJob::Set {
        payload,
        items,
        runtime,
        sftp,
    }) {
        log::error!(target: LOG, "投递剪贴板任务失败:{e}");
    }
}

/// 同步读一次系统剪贴板。**会阻塞 UI 线程**,所以带硬超时:读要走常驻 STA
/// 线程(OLE 的套间规矩),而那一跳的对面是**别人的进程** —— 源程序卡住时
/// `GetData` 可以一直不返回,不设上限的话整个窗口跟着一起僵住。
///
/// 超时(或线程没起来)就返回空 —— 调用方据此退回进程内那份剪贴板。
pub fn read() -> ClipRead {
    let (tx, rx) = std::sync::mpsc::channel();
    if clip_tx().send(ClipJob::Read(tx)).is_err() {
        log::error!(target: LOG, "投递读剪贴板任务失败");
        return ClipRead::default();
    }
    match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(r) => r,
        Err(e) => {
            log::warn!(target: LOG, "读剪贴板超时({e}),按「板上没有我们认识的东西」处理");
            ClipRead::default()
        }
    }
}
