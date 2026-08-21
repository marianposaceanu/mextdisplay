use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, c_char, c_int, c_uint, c_void},
    mem::MaybeUninit,
    ptr, thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_foundation::{NSNumber, NSString};

use crate::state::{SavedDisplay, StateStore};

const MAX_DISPLAYS: u32 = 64;
const CG_ERROR_SUCCESS: i32 = 0;
const CG_CONFIGURE_FOR_SESSION: u32 = 1;
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

type DisplayId = u32;
type DisplayConfig = *mut c_void;
type CfTypeRef = *const c_void;
type CfUuidRef = *const c_void;
type CfStringRef = *const c_void;
type ConfigureDisplayEnabled = unsafe extern "C" fn(DisplayConfig, DisplayId, bool) -> c_int;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayStatus {
    Enabled,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Display {
    pub id: DisplayId,
    pub uuid: String,
    pub name: String,
    pub vendor: u32,
    pub model: u32,
    pub serial: u32,
    pub width: usize,
    pub height: usize,
    pub builtin: bool,
    pub main: bool,
    pub status: DisplayStatus,
}

impl Display {
    pub fn is_external(&self) -> bool {
        !self.builtin
    }
}

#[derive(Debug)]
pub struct DisplayManager {
    store: StateStore,
    session_key: String,
}

impl DisplayManager {
    pub fn new() -> Result<Self> {
        Ok(Self {
            store: StateStore::for_current_user()?,
            session_key: current_session_key()?,
        })
    }

    pub fn refresh(&self) -> Result<Vec<Display>> {
        let mut state = self.store.load()?;
        let original = state.clone();
        let names = screen_names();
        let online = online_displays()?;
        let online_uuids: HashSet<String> =
            online.iter().map(|display| display.uuid.clone()).collect();
        let mut displays = Vec::with_capacity(online.len() + state.displays.len());

        for raw in online {
            let previous_name = state
                .displays
                .get(&raw.uuid)
                .map(|saved| saved.name.as_str());
            let name = names
                .get(&raw.id)
                .cloned()
                .or_else(|| previous_name.map(str::to_owned))
                .unwrap_or_else(|| fallback_name(&raw));

            state.displays.insert(
                raw.uuid.clone(),
                SavedDisplay {
                    display_id: raw.id,
                    name: name.clone(),
                    vendor: raw.vendor,
                    model: raw.model,
                    serial: raw.serial,
                    disabled_session: None,
                },
            );
            displays.push(Display {
                id: raw.id,
                uuid: raw.uuid,
                name,
                vendor: raw.vendor,
                model: raw.model,
                serial: raw.serial,
                width: raw.width,
                height: raw.height,
                builtin: raw.builtin,
                main: raw.main,
                status: DisplayStatus::Enabled,
            });
        }

        for (uuid, saved) in &state.displays {
            if saved.disabled_session.as_deref() == Some(&self.session_key)
                && !online_uuids.contains(uuid.as_str())
            {
                displays.push(Display {
                    id: saved.display_id,
                    uuid: uuid.clone(),
                    name: saved.name.clone(),
                    vendor: saved.vendor,
                    model: saved.model,
                    serial: saved.serial,
                    width: 0,
                    height: 0,
                    builtin: false,
                    main: false,
                    status: DisplayStatus::Disabled,
                });
            }
        }

        displays.sort_by(|left, right| {
            left.builtin
                .cmp(&right.builtin)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.id.cmp(&right.id))
        });

        if state != original {
            self.store.save(&state)?;
        }
        Ok(displays)
    }

    pub fn disable(&self, uuid: &str) -> Result<String> {
        let displays = self.refresh()?;
        let display = displays
            .iter()
            .find(|display| display.uuid == uuid)
            .with_context(|| format!("display {uuid} is not available"))?;

        if display.status == DisplayStatus::Disabled {
            return Ok(format!("{} is already disabled.", display.name));
        }
        if display.builtin {
            bail!("refusing to disable the built-in display");
        }
        if display.main {
            bail!(
                "refusing to disable the main display; make another display main in System Settings first"
            );
        }
        if active_display_count()? < 2 {
            bail!("refusing to disable the only active display");
        }

        self.set_disabled_session(uuid, Some(self.session_key.clone()))?;
        if let Err(error) = configure_display(display.id, false) {
            match wait_for_display(display.id, uuid, false) {
                Ok(None) => {
                    return Ok(format!(
                        "{} is disabled. CoreGraphics reported an error, but the change was verified.",
                        display.name
                    ));
                }
                Ok(Some(_)) => {
                    self.set_disabled_session(uuid, None)?;
                    return Err(error);
                }
                Err(verification_error) => {
                    return Err(error.context(format!(
                        "could not verify the display state: {verification_error:#}; recovery data was kept"
                    )));
                }
            }
        }

        if wait_for_display(display.id, uuid, false)?.is_some() {
            let rollback = configure_display(display.id, true);
            self.set_disabled_session(uuid, None)?;
            match rollback {
                Ok(()) => bail!(
                    "{} remained online; the change was rolled back",
                    display.name
                ),
                Err(rollback_error) => bail!(
                    "{} remained online and rollback failed: {rollback_error:#}",
                    display.name
                ),
            }
        }

        Ok(format!(
            "{} disabled for this login session. Power and USB remain managed by the monitor.",
            display.name
        ))
    }

    pub fn enable(&self, uuid: &str) -> Result<String> {
        let displays = self.refresh()?;
        let display = displays
            .iter()
            .find(|display| display.uuid == uuid)
            .with_context(|| format!("display {uuid} is not available"))?;

        if display.status == DisplayStatus::Enabled {
            return Ok(format!("{} is already enabled.", display.name));
        }

        if let Err(error) = configure_display(display.id, true) {
            match wait_for_display(display.id, uuid, true) {
                Ok(Some(online)) => {
                    self.set_disabled_session(uuid, None)?;
                    return Ok(format!(
                        "{} enabled (display ID {}). CoreGraphics reported an error, but the change was verified.",
                        display.name, online.id
                    ));
                }
                Ok(None) => return Err(error),
                Err(verification_error) => {
                    return Err(error.context(format!(
                        "could not verify the display state: {verification_error:#}; log out or restart to recover"
                    )));
                }
            }
        }
        let online = wait_for_display(display.id, uuid, true)?.with_context(|| {
            format!(
                "{} did not return; log out or restart to restore session display state",
                display.name
            )
        })?;
        self.set_disabled_session(uuid, None)?;

        Ok(format!(
            "{} enabled (display ID {}).",
            display.name, online.id
        ))
    }

    fn set_disabled_session(&self, uuid: &str, session: Option<String>) -> Result<()> {
        let mut state = self.store.load()?;
        let saved = state
            .displays
            .get_mut(uuid)
            .with_context(|| format!("no saved recovery data for display {uuid}"))?;
        saved.disabled_session = session;
        self.store.save(&state)
    }
}

pub fn resolve_display<'a>(displays: &'a [Display], selector: &str) -> Result<&'a Display> {
    let needle = selector.trim();
    if needle.is_empty() {
        bail!("display selector cannot be empty");
    }

    if let Ok(id) = needle.parse::<u32>()
        && let Some(display) = displays.iter().find(|display| display.id == id)
    {
        return Ok(display);
    }

    if let Some(display) = displays
        .iter()
        .find(|display| display.uuid.eq_ignore_ascii_case(needle))
    {
        return Ok(display);
    }

    let lower = needle.to_lowercase();
    let mut matches: Vec<&Display> = displays
        .iter()
        .filter(|display| {
            display.uuid.to_lowercase().starts_with(&lower)
                || display.name.to_lowercase().contains(&lower)
        })
        .collect();
    matches.dedup_by_key(|display| &display.uuid);

    match matches.as_slice() {
        [display] => Ok(display),
        [] => bail!("no display matches '{needle}'"),
        _ => {
            let names = matches
                .iter()
                .map(|display| {
                    let short_uuid: String = display.uuid.chars().take(8).collect();
                    format!("{} ({short_uuid})", display.name)
                })
                .collect::<Vec<_>>()
                .join(", ");
            bail!("display selector '{needle}' is ambiguous: {names}")
        }
    }
}

#[derive(Clone, Debug)]
struct RawDisplay {
    id: DisplayId,
    uuid: String,
    vendor: u32,
    model: u32,
    serial: u32,
    width: usize,
    height: usize,
    builtin: bool,
    main: bool,
}

fn online_displays() -> Result<Vec<RawDisplay>> {
    let ids = online_display_ids()?;
    ids.into_iter().map(raw_display).collect()
}

fn online_display_ids() -> Result<Vec<DisplayId>> {
    let mut ids = [0; MAX_DISPLAYS as usize];
    let mut count = 0;
    // SAFETY: `ids` has capacity for MAX_DISPLAYS entries and `count` is writable.
    let error = unsafe { cg_get_online_display_list(MAX_DISPLAYS, ids.as_mut_ptr(), &mut count) };
    cg_result(error, "enumerate online displays")?;
    Ok(ids[..count as usize].to_vec())
}

fn raw_display(id: DisplayId) -> Result<RawDisplay> {
    // SAFETY: CoreGraphics display queries accept IDs returned by its online display list.
    unsafe {
        Ok(RawDisplay {
            id,
            uuid: display_uuid(id)?,
            vendor: cg_display_vendor_number(id),
            model: cg_display_model_number(id),
            serial: cg_display_serial_number(id),
            width: cg_display_pixels_wide(id),
            height: cg_display_pixels_high(id),
            builtin: cg_display_is_builtin(id) != 0,
            main: cg_main_display_id() == id,
        })
    }
}

fn display_uuid(id: DisplayId) -> Result<String> {
    // SAFETY: The ID came from CoreGraphics. Both Create functions return owned CF objects.
    unsafe {
        let uuid = cg_display_create_uuid(id);
        if uuid.is_null() {
            bail!("CoreGraphics returned no UUID for display {id}");
        }
        let string = cf_uuid_create_string(ptr::null(), uuid);
        cf_release(uuid);
        if string.is_null() {
            bail!("CoreFoundation could not format UUID for display {id}");
        }

        let mut buffer = [0 as c_char; 64];
        let converted = cf_string_get_c_string(
            string,
            buffer.as_mut_ptr(),
            buffer.len() as isize,
            CF_STRING_ENCODING_UTF8,
        );
        cf_release(string);
        if converted == 0 {
            bail!("CoreFoundation could not encode UUID for display {id}");
        }
        Ok(CStr::from_ptr(buffer.as_ptr())
            .to_string_lossy()
            .into_owned())
    }
}

fn screen_names() -> HashMap<DisplayId, String> {
    let Some(marker) = MainThreadMarker::new() else {
        return HashMap::new();
    };
    let key = NSString::from_str("NSScreenNumber");
    let mut names = HashMap::new();

    for screen in NSScreen::screens(marker).to_vec() {
        let description = screen.deviceDescription();
        let Some(value) = description.objectForKey(&key) else {
            continue;
        };
        let Some(number) = value.downcast_ref::<NSNumber>() else {
            continue;
        };
        names.insert(
            number.unsignedIntValue(),
            screen.localizedName().to_string(),
        );
    }
    names
}

fn fallback_name(display: &RawDisplay) -> String {
    if display.builtin {
        return "Built-in Display".to_owned();
    }
    format!(
        "External Display {:04X}:{:04X}",
        display.vendor, display.model
    )
}

fn active_display_count() -> Result<u32> {
    let mut ids = [0; MAX_DISPLAYS as usize];
    let mut count = 0;
    // SAFETY: `ids` has capacity for MAX_DISPLAYS entries and `count` is writable.
    let error = unsafe { cg_get_active_display_list(MAX_DISPLAYS, ids.as_mut_ptr(), &mut count) };
    cg_result(error, "enumerate active displays")?;
    Ok(count)
}

fn configure_display(id: DisplayId, enabled: bool) -> Result<()> {
    let mut configuration: DisplayConfig = ptr::null_mut();
    // SAFETY: CoreGraphics initializes the out pointer on success.
    let error = unsafe { cg_begin_display_configuration(&mut configuration) };
    cg_result(error, "begin display configuration")?;

    let configure = match configure_display_enabled() {
        Ok(configure) => configure,
        Err(error) => {
            // SAFETY: The configuration was successfully created above.
            unsafe { cg_cancel_display_configuration(configuration) };
            return Err(error);
        }
    };

    // SAFETY: The function signature matches the private CoreGraphics symbol and the
    // configuration was created above. `id` is online or was saved in this login session.
    let error = unsafe { configure(configuration, id, enabled) };
    if error != CG_ERROR_SUCCESS {
        // SAFETY: The configuration has not been completed and remains cancellable.
        unsafe { cg_cancel_display_configuration(configuration) };
        return Err(anyhow!(
            "could not {} display {id} (CoreGraphics error {error})",
            if enabled { "enable" } else { "disable" }
        ));
    }

    // SAFETY: The configuration is valid and CoreGraphics consumes it when completed.
    let error =
        unsafe { cg_complete_display_configuration(configuration, CG_CONFIGURE_FOR_SESSION) };
    cg_result(error, "commit display configuration")
}

fn configure_display_enabled() -> Result<ConfigureDisplayEnabled> {
    // SAFETY: dlsym accepts a static NUL-terminated symbol name. The symbol is checked for null.
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"CGSConfigureDisplayEnabled".as_ptr()) };
    if symbol.is_null() {
        bail!(
            "this macOS version does not expose CGSConfigureDisplayEnabled; no display was changed"
        );
    }
    // SAFETY: This signature is verified against the CoreGraphics function used by macOS.
    Ok(unsafe { std::mem::transmute::<*mut c_void, ConfigureDisplayEnabled>(symbol) })
}

fn wait_for_display(id: DisplayId, uuid: &str, online: bool) -> Result<Option<RawDisplay>> {
    for _ in 0..30 {
        let id_is_online = online_display_ids()?.contains(&id);
        if !online && !id_is_online {
            return Ok(None);
        }
        if online && id_is_online {
            match raw_display(id) {
                Ok(display) if display.uuid == uuid => return Ok(Some(display)),
                Ok(display) => bail!(
                    "display ID {id} now belongs to a different display ({})",
                    display.uuid
                ),
                Err(_) => {}
            }
        }
        thread::sleep(Duration::from_millis(100));
    }

    let id_is_online = online_display_ids()?.contains(&id);
    if !id_is_online {
        return Ok(None);
    }
    let display = raw_display(id)?;
    if display.uuid != uuid {
        bail!(
            "display ID {id} now belongs to a different display ({})",
            display.uuid
        );
    }
    Ok(Some(display))
}

fn cg_result(error: i32, action: &str) -> Result<()> {
    if error == CG_ERROR_SUCCESS {
        Ok(())
    } else {
        Err(anyhow!("could not {action} (CoreGraphics error {error})"))
    }
}

fn current_session_key() -> Result<String> {
    let boot = boot_session_uuid()?;
    let mut audit = MaybeUninit::<AuditInfoAddress>::zeroed();
    // SAFETY: `audit` points to a correctly sized writable auditinfo_addr structure.
    let result = unsafe {
        get_audit_address(
            audit.as_mut_ptr(),
            std::mem::size_of::<AuditInfoAddress>() as c_int,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("could not identify login session");
    }
    // SAFETY: getaudit_addr returned success and initialized the structure.
    let audit = unsafe { audit.assume_init() };
    Ok(format!("{boot}:{}", audit.session_id))
}

fn boot_session_uuid() -> Result<String> {
    let mut length = 0;
    // SAFETY: The first sysctlbyname call requests the required output size.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            ptr::null_mut(),
            &mut length,
            ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("could not read boot session UUID");
    }
    let mut buffer = vec![0_u8; length];
    // SAFETY: `buffer` has the size returned by the first sysctlbyname call.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("could not read boot session UUID");
    }
    CStr::from_bytes_until_nul(&buffer)
        .context("boot session UUID was not NUL-terminated")?
        .to_str()
        .context("boot session UUID was not UTF-8")
        .map(str::to_owned)
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AuditMask {
    success: c_uint,
    failure: c_uint,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AuditTerminalAddress {
    port: libc::dev_t,
    address_type: c_uint,
    address: [c_uint; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AuditInfoAddress {
    audit_user_id: libc::uid_t,
    mask: AuditMask,
    terminal_id: AuditTerminalAddress,
    session_id: libc::pid_t,
    flags: u64,
}

unsafe extern "C" {
    #[link_name = "getaudit_addr"]
    fn get_audit_address(info: *mut AuditInfoAddress, length: c_int) -> c_int;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    #[link_name = "CGGetOnlineDisplayList"]
    fn cg_get_online_display_list(
        max_displays: u32,
        displays: *mut DisplayId,
        count: *mut u32,
    ) -> c_int;
    #[link_name = "CGGetActiveDisplayList"]
    fn cg_get_active_display_list(
        max_displays: u32,
        displays: *mut DisplayId,
        count: *mut u32,
    ) -> c_int;
    #[link_name = "CGMainDisplayID"]
    fn cg_main_display_id() -> DisplayId;
    #[link_name = "CGDisplayIsBuiltin"]
    fn cg_display_is_builtin(display: DisplayId) -> c_uint;
    #[link_name = "CGDisplayVendorNumber"]
    fn cg_display_vendor_number(display: DisplayId) -> u32;
    #[link_name = "CGDisplayModelNumber"]
    fn cg_display_model_number(display: DisplayId) -> u32;
    #[link_name = "CGDisplaySerialNumber"]
    fn cg_display_serial_number(display: DisplayId) -> u32;
    #[link_name = "CGDisplayPixelsWide"]
    fn cg_display_pixels_wide(display: DisplayId) -> usize;
    #[link_name = "CGDisplayPixelsHigh"]
    fn cg_display_pixels_high(display: DisplayId) -> usize;
    #[link_name = "CGDisplayCreateUUIDFromDisplayID"]
    fn cg_display_create_uuid(display: DisplayId) -> CfUuidRef;
    #[link_name = "CGBeginDisplayConfiguration"]
    fn cg_begin_display_configuration(configuration: *mut DisplayConfig) -> c_int;
    #[link_name = "CGCancelDisplayConfiguration"]
    fn cg_cancel_display_configuration(configuration: DisplayConfig) -> c_int;
    #[link_name = "CGCompleteDisplayConfiguration"]
    fn cg_complete_display_configuration(configuration: DisplayConfig, option: u32) -> c_int;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    #[link_name = "CFUUIDCreateString"]
    fn cf_uuid_create_string(allocator: *const c_void, uuid: CfUuidRef) -> CfStringRef;
    #[link_name = "CFStringGetCString"]
    fn cf_string_get_c_string(
        string: CfStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
    #[link_name = "CFRelease"]
    fn cf_release(value: CfTypeRef);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(id: u32, uuid: &str, name: &str) -> Display {
        Display {
            id,
            uuid: uuid.to_owned(),
            name: name.to_owned(),
            vendor: 0,
            model: 0,
            serial: 0,
            width: 0,
            height: 0,
            builtin: false,
            main: false,
            status: DisplayStatus::Enabled,
        }
    }

    #[test]
    fn resolves_id_uuid_prefix_and_name() {
        let displays = vec![
            display(2, "AAAAAAAA-1111-2222-3333-444444444444", "Studio Display"),
            display(3, "BBBBBBBB-1111-2222-3333-444444444444", "Desk Display"),
        ];

        assert_eq!(resolve_display(&displays, "2").unwrap().id, 2);
        assert_eq!(resolve_display(&displays, "bbbbbbbb").unwrap().id, 3);
        assert_eq!(resolve_display(&displays, "studio").unwrap().id, 2);
        assert!(resolve_display(&displays, "display").is_err());
    }
}
