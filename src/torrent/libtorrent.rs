use super::*;
use anyhow::{Context, Result};
use std::{
    ffi::{c_char, c_void, CStr, CString},
    path::Path,
    sync::{Arc, Mutex},
};

unsafe extern "C" {
    fn ts_create(source: *const c_char, dir: *const c_char, limit: i32) -> *mut c_void;
    fn ts_destroy(engine: *mut c_void);
    fn ts_error() -> *const c_char;
    fn ts_free(p: *mut c_char);
    fn ts_metadata(engine: *mut c_void) -> *mut c_char;
    fn ts_select(engine: *mut c_void, file: i32) -> i32;
    fn ts_status(engine: *mut c_void) -> *mut c_char;
    fn ts_priorities(engine: *mut c_void, updates: *const PriorityUpdate, len: usize) -> i32;
}
struct Native(*mut c_void);
// The pointer is exclusively accessed under Mutex, and libtorrent marshals calls to its thread.
unsafe impl Send for Native {}
impl Drop for Native {
    fn drop(&mut self) {
        unsafe { ts_destroy(self.0) };
    }
}
#[derive(Clone)]
pub struct Libtorrent(Arc<Mutex<Native>>);
fn native_error() -> anyhow::Error {
    // Called on the same blocking thread as the failed native call (thread-local error).
    anyhow::anyhow!(
        "libtorrent: {}",
        unsafe { CStr::from_ptr(ts_error()) }.to_string_lossy()
    )
}
fn json<T: serde::de::DeserializeOwned>(ptr: *mut c_char) -> Result<T> {
    if ptr.is_null() {
        return Err(native_error());
    }
    let result = serde_json::from_slice(unsafe { CStr::from_ptr(ptr) }.to_bytes());
    unsafe { ts_free(ptr) };
    result.context("invalid backend response")
}
impl Libtorrent {
    pub async fn open(source: String, dir: &Path, limit: u32) -> Result<Self> {
        use std::os::unix::ffi::OsStrExt;
        let source = CString::new(source)?;
        let dir = CString::new(dir.as_os_str().as_bytes())?;
        let limit = i32::try_from(limit).context("download rate limit is too large")?;
        tokio::task::spawn_blocking(move || {
            let ptr = unsafe { ts_create(source.as_ptr(), dir.as_ptr(), limit) };
            if ptr.is_null() {
                return Err(native_error());
            }
            Ok(Self(Arc::new(Mutex::new(Native(ptr)))))
        })
        .await?
    }
    async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(*mut c_void) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let engine = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let native = engine
                .lock()
                .map_err(|_| anyhow::anyhow!("backend lock poisoned"))?;
            f(native.0)
        })
        .await?
    }
}
#[cfg(feature = "integration-tests")]
unsafe extern "C" {
    fn ts_listen_port(engine: *mut c_void) -> i32;
    fn ts_magnet(engine: *mut c_void) -> *mut c_char;
    fn ts_connect(engine: *mut c_void, host: *const c_char, port: u16) -> i32;
    fn ts_priority_snapshot(engine: *mut c_void) -> *mut c_char;
    fn ts_make_fixture(root: *const c_char, name: *const c_char, output: *const c_char) -> i32;
}
#[cfg(feature = "integration-tests")]
impl Libtorrent {
    pub async fn magnet(&self) -> Result<String> {
        self.call(|p| {
            let ptr = unsafe { ts_magnet(p) };
            if ptr.is_null() {
                return Err(native_error());
            }
            let value = unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned();
            unsafe { ts_free(ptr) };
            Ok(value)
        })
        .await
    }
    pub async fn listen_port(&self) -> Result<u16> {
        self.call(|p| Ok(unsafe { ts_listen_port(p) } as u16)).await
    }
    pub async fn connect_peer(&self, host: &str, port: u16) -> Result<()> {
        let host = CString::new(host)?;
        self.call(move |p| {
            if unsafe { ts_connect(p, host.as_ptr(), port) } == 0 {
                Ok(())
            } else {
                Err(native_error())
            }
        })
        .await
    }
    pub async fn priority_snapshot(&self) -> Result<Vec<u32>> {
        self.call(|p| json(unsafe { ts_priority_snapshot(p) }))
            .await
    }
    pub async fn make_fixture(root: &Path, name: &str, output: &Path) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let root = CString::new(root.as_os_str().as_bytes())?;
        let output = CString::new(output.as_os_str().as_bytes())?;
        let name = CString::new(name)?;
        tokio::task::spawn_blocking(move || {
            if unsafe { ts_make_fixture(root.as_ptr(), name.as_ptr(), output.as_ptr()) } == 0 {
                Ok(())
            } else {
                Err(native_error())
            }
        })
        .await?
    }
}
impl TorrentBackend for Libtorrent {
    async fn metadata(&self) -> Result<Option<Metadata>> {
        self.call(|p| json(unsafe { ts_metadata(p) })).await
    }
    async fn select(&self, file: usize) -> Result<()> {
        let file = i32::try_from(file)?;
        self.call(move |p| {
            if unsafe { ts_select(p, file) } == 0 {
                Ok(())
            } else {
                Err(native_error())
            }
        })
        .await
    }
    async fn priorities(&self, updates: Vec<PriorityUpdate>) -> Result<()> {
        self.call(move |p| {
            if unsafe { ts_priorities(p, updates.as_ptr(), updates.len()) } == 0 {
                Ok(())
            } else {
                Err(native_error())
            }
        })
        .await
    }
    async fn status(&self) -> Result<TorrentStatus> {
        self.call(|p| json(unsafe { ts_status(p) })).await
    }
}
