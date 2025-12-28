#![allow(unsafe_code)]

use crate::SvnError;
use base64::Engine;
use libloading::Library;
use std::alloc::{Layout, alloc, dealloc};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_uint, c_ulong, c_void};
use std::ptr;
use std::sync::OnceLock;

pub(crate) const SASL_CONTINUE: c_int = 1;

const SASL_OK: c_int = 0;
const SASL_INTERACT: c_int = 2;
const SASL_FAIL: c_int = -1;
const SASL_NOMEM: c_int = -2;
const SASL_NOMECH: c_int = -4;
const SASL_BADPARAM: c_int = -7;

const SASL_SUCCESS_DATA: c_uint = 0x0004;

const SASL_SSF: c_int = 1;
const SASL_MAXOUTBUF: c_int = 2;

const SASL_SEC_PROPS: c_int = 101;
const SASL_AUTH_EXTERNAL: c_int = 102;

const SASL_CB_LIST_END: c_ulong = 0;
const SASL_CB_AUTHNAME: c_ulong = 0x4002;
const SASL_CB_PASS: c_ulong = 0x4004;

const SASL_SERVICE: &str = "svn";
const SASL_MAX_SSF: u32 = 256;
const SASL_DEFAULT_MAX_BUFSIZE: u32 = 4096 * 4;

type SaslSsfT = c_uint;

#[repr(C)]
struct sasl_conn_t {
    _private: [u8; 0],
}

#[repr(C)]
struct sasl_secret_t {
    len: c_ulong,
    data: [u8; 1],
}

#[repr(C)]
struct sasl_security_properties_t {
    min_ssf: SaslSsfT,
    max_ssf: SaslSsfT,
    maxbufsize: c_uint,
    security_flags: c_uint,
    property_names: *const *const c_char,
    property_values: *const *const c_char,
}

#[repr(C)]
struct sasl_interact_t {
    id: c_ulong,
    challenge: *const c_char,
    prompt: *const c_char,
    defresult: *const c_char,
    result: *const c_void,
    len: c_uint,
}

#[repr(C)]
struct sasl_callback_t {
    id: c_ulong,
    proc_: Option<unsafe extern "C" fn() -> c_int>,
    context: *mut c_void,
}

type SaslClientInit = unsafe extern "C" fn(*const sasl_callback_t) -> c_int;
type SaslClientNew = unsafe extern "C" fn(
    service: *const c_char,
    server_fqdn: *const c_char,
    iplocalport: *const c_char,
    ipremoteport: *const c_char,
    prompt_supp: *const sasl_callback_t,
    flags: c_uint,
    pconn: *mut *mut sasl_conn_t,
) -> c_int;
type SaslClientStart = unsafe extern "C" fn(
    conn: *mut sasl_conn_t,
    mechlist: *const c_char,
    prompt_need: *mut *mut sasl_interact_t,
    clientout: *mut *const c_char,
    clientoutlen: *mut c_uint,
    mech: *mut *const c_char,
) -> c_int;
type SaslClientStep = unsafe extern "C" fn(
    conn: *mut sasl_conn_t,
    serverin: *const c_char,
    serverinlen: c_uint,
    prompt_need: *mut *mut sasl_interact_t,
    clientout: *mut *const c_char,
    clientoutlen: *mut c_uint,
) -> c_int;
type SaslEncode = unsafe extern "C" fn(
    conn: *mut sasl_conn_t,
    input: *const c_char,
    inputlen: c_uint,
    output: *mut *const c_char,
    outputlen: *mut c_uint,
) -> c_int;
type SaslDecode = unsafe extern "C" fn(
    conn: *mut sasl_conn_t,
    input: *const c_char,
    inputlen: c_uint,
    output: *mut *const c_char,
    outputlen: *mut c_uint,
) -> c_int;
type SaslDispose = unsafe extern "C" fn(pconn: *mut *mut sasl_conn_t);
type SaslGetProp = unsafe extern "C" fn(
    conn: *mut sasl_conn_t,
    propnum: c_int,
    pvalue: *mut *const c_void,
) -> c_int;
type SaslSetProp =
    unsafe extern "C" fn(conn: *mut sasl_conn_t, propnum: c_int, value: *const c_void) -> c_int;
type SaslErrString = unsafe extern "C" fn(
    saslerr: c_int,
    langlist: *const c_char,
    outlang: *mut *const c_char,
) -> *const c_char;
type SaslErrDetail = unsafe extern "C" fn(conn: *mut sasl_conn_t) -> *const c_char;

struct SaslApi {
    _lib: Library,
    sasl_client_init: SaslClientInit,
    sasl_client_new: SaslClientNew,
    sasl_client_start: SaslClientStart,
    sasl_client_step: SaslClientStep,
    sasl_encode: SaslEncode,
    sasl_decode: SaslDecode,
    sasl_dispose: SaslDispose,
    sasl_getprop: SaslGetProp,
    sasl_setprop: SaslSetProp,
    sasl_errstring: SaslErrString,
    sasl_errdetail: SaslErrDetail,
}

static SASL_API: OnceLock<Result<SaslApi, String>> = OnceLock::new();

#[cfg(windows)]
const SASL_LIB_NAMES: &[&str] = &["libsasl.dll", "libsasl2.dll", "sasl2.dll"];

#[cfg(target_os = "macos")]
const SASL_LIB_NAMES: &[&str] = &["libsasl2.2.dylib", "libsasl2.dylib"];

#[cfg(all(unix, not(target_os = "macos")))]
const SASL_LIB_NAMES: &[&str] = &["libsasl2.so.2", "libsasl2.so"];

#[cfg(not(any(windows, unix)))]
const SASL_LIB_NAMES: &[&str] = &[];

fn sasl_api() -> Result<&'static SaslApi, SvnError> {
    let api = SASL_API.get_or_init(SaslApi::load);
    match api {
        Ok(api) => Ok(api),
        Err(_) => Err(SvnError::AuthUnavailable),
    }
}

fn load_library() -> Result<Library, String> {
    let mut last_err = None::<String>;
    for &name in SASL_LIB_NAMES {
        match unsafe { Library::new(name) } {
            Ok(lib) => return Ok(lib),
            Err(err) => last_err = Some(format!("{name}: {err}")),
        }
    }
    Err(last_err.unwrap_or_else(|| "unable to load Cyrus SASL library".to_string()))
}

unsafe fn load_sym<T: Copy>(lib: &Library, name: &'static [u8]) -> Result<T, String> {
    let symbol = unsafe { lib.get::<T>(name) }.map_err(|err| {
        let name = String::from_utf8_lossy(name);
        format!("missing SASL symbol {name}: {err}")
    })?;
    Ok(*symbol)
}

impl SaslApi {
    fn load() -> Result<Self, String> {
        let lib = load_library()?;

        let sasl_client_init = unsafe { load_sym::<SaslClientInit>(&lib, b"sasl_client_init\0")? };
        let sasl_client_new = unsafe { load_sym::<SaslClientNew>(&lib, b"sasl_client_new\0")? };
        let sasl_client_start =
            unsafe { load_sym::<SaslClientStart>(&lib, b"sasl_client_start\0")? };
        let sasl_client_step = unsafe { load_sym::<SaslClientStep>(&lib, b"sasl_client_step\0")? };
        let sasl_encode = unsafe { load_sym::<SaslEncode>(&lib, b"sasl_encode\0")? };
        let sasl_decode = unsafe { load_sym::<SaslDecode>(&lib, b"sasl_decode\0")? };
        let sasl_dispose = unsafe { load_sym::<SaslDispose>(&lib, b"sasl_dispose\0")? };
        let sasl_getprop = unsafe { load_sym::<SaslGetProp>(&lib, b"sasl_getprop\0")? };
        let sasl_setprop = unsafe { load_sym::<SaslSetProp>(&lib, b"sasl_setprop\0")? };
        let sasl_errstring = unsafe { load_sym::<SaslErrString>(&lib, b"sasl_errstring\0")? };
        let sasl_errdetail = unsafe { load_sym::<SaslErrDetail>(&lib, b"sasl_errdetail\0")? };

        let api = Self {
            _lib: lib,
            sasl_client_init,
            sasl_client_new,
            sasl_client_start,
            sasl_client_step,
            sasl_encode,
            sasl_decode,
            sasl_dispose,
            sasl_getprop,
            sasl_setprop,
            sasl_errstring,
            sasl_errdetail,
        };

        let rc = unsafe { (api.sasl_client_init)(ptr::null()) };
        if rc != SASL_OK {
            return Err(api.error_string(rc, None));
        }

        Ok(api)
    }

    fn error_string(&self, code: c_int, conn: Option<*mut sasl_conn_t>) -> String {
        let err_ptr = unsafe { (self.sasl_errstring)(code, ptr::null(), ptr::null_mut()) };
        let err = if err_ptr.is_null() {
            format!("SASL error {code}")
        } else {
            unsafe { CStr::from_ptr(err_ptr) }
                .to_string_lossy()
                .into_owned()
        };
        let detail_ptr = conn.map(|conn| unsafe { (self.sasl_errdetail)(conn) });
        let detail = detail_ptr.filter(|ptr| !ptr.is_null()).map(|ptr| {
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
        });
        match detail {
            Some(detail) if !detail.is_empty() => format!("{err}: {detail}"),
            _ => err,
        }
    }
}

struct SaslSecret {
    ptr: *mut sasl_secret_t,
    layout: Layout,
}

impl SaslSecret {
    fn new(password: &[u8]) -> Result<Self, SvnError> {
        let size = std::mem::size_of::<sasl_secret_t>() + password.len().saturating_sub(1);
        let layout = Layout::from_size_align(size, std::mem::align_of::<sasl_secret_t>())
            .map_err(|_| SvnError::Protocol("invalid SASL secret layout".into()))?;
        let ptr = unsafe { alloc(layout) } as *mut sasl_secret_t;
        if ptr.is_null() {
            return Err(SvnError::Protocol("failed to allocate SASL secret".into()));
        }

        unsafe {
            (*ptr).len = password.len() as c_ulong;
            if !password.is_empty() {
                ptr::copy_nonoverlapping(
                    password.as_ptr(),
                    (*ptr).data.as_mut_ptr(),
                    password.len(),
                );
            }
        }

        Ok(Self { ptr, layout })
    }

    fn as_mut_ptr(&mut self) -> *mut sasl_secret_t {
        self.ptr
    }
}

impl Drop for SaslSecret {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr.cast::<u8>(), self.layout) };
    }
}

struct Credentials {
    username: Option<CString>,
    password: Option<Vec<u8>>,
    secret: Option<SaslSecret>,
}

impl Credentials {
    fn new(username: Option<&str>, password: Option<&str>) -> Result<Self, SvnError> {
        let username = match username {
            Some(name) if !name.trim().is_empty() => Some(
                CString::new(name)
                    .map_err(|_| SvnError::Protocol("username contains NUL byte".into()))?,
            ),
            _ => None,
        };
        let password = password.map(|p| p.as_bytes().to_vec());
        Ok(Self {
            username,
            password,
            secret: None,
        })
    }
}

unsafe extern "C" fn get_username_cb(
    context: *mut c_void,
    _id: c_int,
    result: *mut *const c_char,
    len: *mut c_uint,
) -> c_int {
    let creds = unsafe { &mut *context.cast::<Credentials>() };
    let Some(username) = creds.username.as_ref() else {
        return SASL_FAIL;
    };
    unsafe {
        *result = username.as_ptr();
        if !len.is_null() {
            *len = username.as_bytes().len() as c_uint;
        }
    }
    SASL_OK
}

unsafe extern "C" fn get_password_cb(
    _conn: *mut sasl_conn_t,
    context: *mut c_void,
    _id: c_int,
    psecret: *mut *mut sasl_secret_t,
) -> c_int {
    let creds = unsafe { &mut *context.cast::<Credentials>() };
    let Some(password) = creds.password.as_ref() else {
        return SASL_FAIL;
    };

    if creds.secret.is_none() {
        match SaslSecret::new(password) {
            Ok(secret) => creds.secret = Some(secret),
            Err(_) => return SASL_FAIL,
        }
    }
    let Some(secret) = creds.secret.as_mut() else {
        return SASL_FAIL;
    };

    unsafe { *psecret = secret.as_mut_ptr() };
    SASL_OK
}

fn callback_proc_getsimple(
    func: unsafe extern "C" fn(*mut c_void, c_int, *mut *const c_char, *mut c_uint) -> c_int,
) -> Option<unsafe extern "C" fn() -> c_int> {
    Some(unsafe {
        std::mem::transmute::<
            unsafe extern "C" fn(*mut c_void, c_int, *mut *const c_char, *mut c_uint) -> c_int,
            unsafe extern "C" fn() -> c_int,
        >(func)
    })
}

fn callback_proc_getsecret(
    func: unsafe extern "C" fn(
        *mut sasl_conn_t,
        *mut c_void,
        c_int,
        *mut *mut sasl_secret_t,
    ) -> c_int,
) -> Option<unsafe extern "C" fn() -> c_int> {
    Some(unsafe {
        std::mem::transmute::<
            unsafe extern "C" fn(
                *mut sasl_conn_t,
                *mut c_void,
                c_int,
                *mut *mut sasl_secret_t,
            ) -> c_int,
            unsafe extern "C" fn() -> c_int,
        >(func)
    })
}

/// Client-side Cyrus SASL context used for authentication and (optionally) a
/// negotiated SASL security layer.
pub(crate) struct CyrusSasl {
    api: &'static SaslApi,
    conn: *mut sasl_conn_t,
    _service: CString,
    _hostname: CString,
    _local_addrport: Option<CString>,
    _remote_addrport: Option<CString>,
    _external_auth: Option<CString>,
    _creds: Box<Credentials>,
    _callbacks: Box<[sasl_callback_t; 3]>,
    max_outbuf: u32,
}

unsafe impl Send for CyrusSasl {}

impl CyrusSasl {
    pub(crate) fn new(
        hostname: &str,
        username: Option<&str>,
        password: Option<&str>,
        tunneled: bool,
        local_addrport: Option<&str>,
        remote_addrport: Option<&str>,
    ) -> Result<Self, SvnError> {
        let api = sasl_api()?;

        let service = CString::new(SASL_SERVICE)
            .map_err(|_| SvnError::Protocol("SASL service contains NUL byte".into()))?;
        let hostname = CString::new(hostname)
            .map_err(|_| SvnError::Protocol("SASL hostname contains NUL byte".into()))?;
        let local_addrport = local_addrport
            .map(CString::new)
            .transpose()
            .map_err(|_| SvnError::Protocol("SASL local addrport contains NUL byte".into()))?;
        let remote_addrport = remote_addrport
            .map(CString::new)
            .transpose()
            .map_err(|_| SvnError::Protocol("SASL remote addrport contains NUL byte".into()))?;
        let mut creds = Box::new(Credentials::new(username, password)?);

        let callbacks = Box::new([
            sasl_callback_t {
                id: SASL_CB_AUTHNAME,
                proc_: callback_proc_getsimple(get_username_cb),
                context: creds.as_mut() as *mut Credentials as *mut c_void,
            },
            sasl_callback_t {
                id: SASL_CB_PASS,
                proc_: callback_proc_getsecret(get_password_cb),
                context: creds.as_mut() as *mut Credentials as *mut c_void,
            },
            sasl_callback_t {
                id: SASL_CB_LIST_END,
                proc_: None,
                context: ptr::null_mut(),
            },
        ]);

        let mut conn: *mut sasl_conn_t = ptr::null_mut();
        let iplocalport = local_addrport.as_ref().map_or(ptr::null(), |s| s.as_ptr());
        let ipremoteport = remote_addrport.as_ref().map_or(ptr::null(), |s| s.as_ptr());
        let rc = unsafe {
            (api.sasl_client_new)(
                service.as_ptr(),
                hostname.as_ptr(),
                iplocalport,
                ipremoteport,
                callbacks.as_ref().as_ptr(),
                SASL_SUCCESS_DATA,
                &mut conn,
            )
        };
        if rc != SASL_OK {
            return Err(SvnError::AuthUnavailable);
        }

        let mut external_auth = None;
        if tunneled {
            let value = CString::new(" ").map_err(|_| {
                SvnError::Protocol("SASL external auth value contains NUL byte".into())
            })?;
            let rc = unsafe {
                (api.sasl_setprop)(conn, SASL_AUTH_EXTERNAL, value.as_ptr().cast::<c_void>())
            };
            if rc != SASL_OK {
                return Err(SvnError::AuthFailed(api.error_string(rc, Some(conn))));
            }
            external_auth = Some(value);
        }

        let secprops = sasl_security_properties_t {
            min_ssf: 0,
            max_ssf: SASL_MAX_SSF,
            maxbufsize: SASL_DEFAULT_MAX_BUFSIZE,
            security_flags: 0,
            property_names: ptr::null(),
            property_values: ptr::null(),
        };
        let rc = unsafe {
            (api.sasl_setprop)(
                conn,
                SASL_SEC_PROPS,
                (&secprops as *const sasl_security_properties_t).cast(),
            )
        };
        if rc != SASL_OK {
            return Err(SvnError::AuthFailed(api.error_string(rc, Some(conn))));
        }

        let max_outbuf = get_prop_u32(api, conn, SASL_MAXOUTBUF)?.unwrap_or(0);

        Ok(Self {
            api,
            conn,
            _service: service,
            _hostname: hostname,
            _local_addrport: local_addrport,
            _remote_addrport: remote_addrport,
            _external_auth: external_auth,
            _creds: creds,
            _callbacks: callbacks,
            max_outbuf,
        })
    }

    pub(crate) fn max_outbuf(&self) -> u32 {
        self.max_outbuf
    }

    pub(crate) fn ssf(&self) -> Result<u32, SvnError> {
        get_prop_u32(self.api, self.conn, SASL_SSF).map(|v| v.unwrap_or(0))
    }

    pub(crate) fn client_start(
        &mut self,
        mechlist: &CStr,
    ) -> Result<(String, Option<Vec<u8>>, c_int), SvnError> {
        let mut interact: *mut sasl_interact_t = ptr::null_mut();
        let mut out: *const c_char = ptr::null();
        let mut outlen: c_uint = 0;
        let mut mech: *const c_char = ptr::null();

        let rc = unsafe {
            (self.api.sasl_client_start)(
                self.conn,
                mechlist.as_ptr(),
                &mut interact,
                &mut out,
                &mut outlen,
                &mut mech,
            )
        };
        match rc {
            SASL_OK | SASL_CONTINUE => {}
            SASL_NOMECH | SASL_INTERACT => return Err(SvnError::AuthUnavailable),
            SASL_BADPARAM | SASL_NOMEM => {
                return Err(SvnError::AuthFailed(
                    self.api.error_string(rc, Some(self.conn)),
                ));
            }
            _ => return Err(SvnError::AuthUnavailable),
        }

        if mech.is_null() {
            return Err(SvnError::Protocol(
                "SASL did not report selected mechanism".into(),
            ));
        }
        let mech = unsafe { CStr::from_ptr(mech) }
            .to_string_lossy()
            .into_owned();
        let initial = if outlen > 0 {
            let bytes = unsafe { std::slice::from_raw_parts(out.cast::<u8>(), outlen as usize) };
            Some(bytes.to_vec())
        } else {
            None
        };

        Ok((mech, initial, rc))
    }

    pub(crate) fn client_step(
        &mut self,
        server_in: &[u8],
    ) -> Result<(Option<Vec<u8>>, c_int), SvnError> {
        let mut server_buf = Vec::with_capacity(server_in.len() + 1);
        server_buf.extend_from_slice(server_in);
        server_buf.push(0);

        let mut interact: *mut sasl_interact_t = ptr::null_mut();
        let mut out: *const c_char = ptr::null();
        let mut outlen: c_uint = 0;

        let rc = unsafe {
            (self.api.sasl_client_step)(
                self.conn,
                server_buf.as_ptr().cast::<c_char>(),
                server_in.len() as c_uint,
                &mut interact,
                &mut out,
                &mut outlen,
            )
        };
        if rc != SASL_OK && rc != SASL_CONTINUE {
            return Err(SvnError::AuthFailed(
                self.api.error_string(rc, Some(self.conn)),
            ));
        }

        let out = if outlen > 0 {
            let bytes = unsafe { std::slice::from_raw_parts(out.cast::<u8>(), outlen as usize) };
            Some(bytes.to_vec())
        } else {
            None
        };

        Ok((out, rc))
    }

    pub(crate) fn encode(&mut self, input: &[u8]) -> Result<Vec<u8>, SvnError> {
        let mut out: *const c_char = ptr::null();
        let mut outlen: c_uint = 0;
        let rc = unsafe {
            (self.api.sasl_encode)(
                self.conn,
                input.as_ptr().cast::<c_char>(),
                input.len() as c_uint,
                &mut out,
                &mut outlen,
            )
        };
        if rc != SASL_OK {
            return Err(SvnError::Protocol(
                self.api.error_string(rc, Some(self.conn)),
            ));
        }
        let bytes = unsafe { std::slice::from_raw_parts(out.cast::<u8>(), outlen as usize) };
        Ok(bytes.to_vec())
    }

    pub(crate) fn decode(&mut self, input: &[u8]) -> Result<Vec<u8>, SvnError> {
        let mut out: *const c_char = ptr::null();
        let mut outlen: c_uint = 0;
        let rc = unsafe {
            (self.api.sasl_decode)(
                self.conn,
                input.as_ptr().cast::<c_char>(),
                input.len() as c_uint,
                &mut out,
                &mut outlen,
            )
        };
        if rc != SASL_OK {
            return Err(SvnError::Protocol(
                self.api.error_string(rc, Some(self.conn)),
            ));
        }
        let bytes = unsafe { std::slice::from_raw_parts(out.cast::<u8>(), outlen as usize) };
        Ok(bytes.to_vec())
    }
}

impl Drop for CyrusSasl {
    fn drop(&mut self) {
        unsafe {
            (self.api.sasl_dispose)(&mut self.conn);
        }
    }
}

fn get_prop_u32(
    api: &SaslApi,
    conn: *mut sasl_conn_t,
    prop: c_int,
) -> Result<Option<u32>, SvnError> {
    let mut value: *const c_void = ptr::null();
    let rc = unsafe { (api.sasl_getprop)(conn, prop, &mut value) };
    if rc != SASL_OK {
        return Err(SvnError::Protocol(api.error_string(rc, Some(conn))));
    }
    if value.is_null() {
        return Ok(None);
    }
    Ok(Some(unsafe { *(value.cast::<u32>()) }))
}

pub(crate) fn base64_encode(data: &[u8]) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .encode(data)
        .into_bytes()
}

pub(crate) fn base64_decode(data: &[u8]) -> Result<Vec<u8>, SvnError> {
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| SvnError::Protocol("invalid base64 SASL token".into()))
}
