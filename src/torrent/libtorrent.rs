use super::*;
use anyhow::{Context, Result};
use std::{
    ffi::{c_char, c_void, CStr, CString},
    path::Path,
    sync::{Arc, Mutex},
};

unsafe extern "C" {
    fn ts_source_key(source: *const c_char) -> *mut c_char;
    fn ts_recheck(engine: *mut c_void) -> i32;
    fn ts_create(
        source: *const c_char,
        dir: *const c_char,
        limit: i32,
        cached: *const c_char,
    ) -> *mut c_void;
    fn ts_destroy(engine: *mut c_void);
    fn ts_error() -> *const c_char;
    fn ts_free(p: *mut c_char);
    fn ts_export_metadata(engine: *mut c_void) -> *mut c_char;
    fn ts_metadata(engine: *mut c_void) -> *mut c_char;
    fn ts_select(engine: *mut c_void, file: i32) -> i32;
    fn ts_status(engine: *mut c_void) -> *mut c_char;
    fn ts_priorities(engine: *mut c_void, updates: *const PriorityUpdate, len: usize) -> i32;
}
struct Native(*mut c_void, Option<Arc<crate::downloads::DownloadCache>>);
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
fn native_path(path: &Path) -> Result<CString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(CString::new(path.as_os_str().as_bytes())?)
    }
    #[cfg(windows)]
    {
        // libtorrent's Windows API accepts UTF-8 and converts it to UTF-16.
        Ok(CString::new(
            path.to_str().context("path must be valid Unicode")?,
        )?)
    }
}
impl Libtorrent {
    /// Keep cache locks until the final native handle, including in-flight
    /// blocking calls, has destroyed the engine and stopped its disk workers.
    pub fn retain_cache(&self, cache: Arc<crate::downloads::DownloadCache>) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("backend lock poisoned"))?
            .1 = Some(cache);
        Ok(())
    }
    pub async fn source_key(source: String) -> Result<String> {
        let source = CString::new(source)?;
        tokio::task::spawn_blocking(move || {
            let ptr = unsafe { ts_source_key(source.as_ptr()) };
            if ptr.is_null() {
                return Err(native_error());
            }
            let key = unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned();
            unsafe { ts_free(ptr) };
            Ok(key)
        })
        .await?
    }
    pub async fn recheck(&self) -> Result<()> {
        self.call(|p| {
            if unsafe { ts_recheck(p) } == 0 {
                Ok(())
            } else {
                Err(native_error())
            }
        })
        .await
    }
    pub async fn open(source: String, dir: &Path, limit: u32) -> Result<Self> {
        Self::open_cached(source, dir, limit, None).await
    }
    pub async fn open_cached(
        source: String,
        dir: &Path,
        limit: u32,
        cached: Option<&Path>,
    ) -> Result<Self> {
        let cached = cached.map(native_path).transpose()?;
        let source = CString::new(source)?;
        let dir = native_path(dir)?;
        let limit = i32::try_from(limit).context("download rate limit is too large")?;
        tokio::task::spawn_blocking(move || {
            let ptr = unsafe {
                ts_create(
                    source.as_ptr(),
                    dir.as_ptr(),
                    limit,
                    cached.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                )
            };
            if ptr.is_null() {
                return Err(native_error());
            }
            Ok(Self(Arc::new(Mutex::new(Native(ptr, None)))))
        })
        .await?
    }
    pub async fn export_metadata(&self) -> Result<Vec<u8>> {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(bytes) = self
                    .call(|p| json::<Option<Vec<u8>>>(unsafe { ts_export_metadata(p) }))
                    .await?
                {
                    return Ok(bytes);
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .context("save torrent metadata timeout")?
    }
    async fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(*mut c_void) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let engine = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let result = engine
                .lock()
                .map_err(|_| anyhow::anyhow!("backend lock poisoned"))
                .and_then(|native| f(native.0));
            // Release the captured handle before Tokio publishes completion.
            // Otherwise dropping the caller's last handle can briefly leave
            // the cache locked by this already-completed blocking task.
            drop(engine);
            result
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
    fn ts_make_fixture(
        root: *const c_char,
        name: *const c_char,
        output: *const c_char,
        version: i32,
    ) -> i32;
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
        Self::make_fixture_version(root, name, output, 1).await
    }
    /// Integration fixture: 1 = v1, 2 = v2, 3 = hybrid.
    pub async fn make_fixture_version(
        root: &Path,
        name: &str,
        output: &Path,
        version: i32,
    ) -> Result<()> {
        anyhow::ensure!((1..=3).contains(&version), "invalid fixture version");
        let root = native_path(root)?;
        let output = native_path(output)?;
        let name = CString::new(name)?;
        tokio::task::spawn_blocking(move || {
            if unsafe { ts_make_fixture(root.as_ptr(), name.as_ptr(), output.as_ptr(), version) }
                == 0
            {
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
