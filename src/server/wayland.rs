use super::*;
use hbb_common::{
    allow_err,
    platform::linux::{CMD_SH, DISTRO},
};
use scrap::{is_cursor_embedded, set_map_err, Capturer, Display, Frame, TraitCapturer};
use std::collections::HashMap;
use std::io;
use std::process::{Command, Output};

use crate::{
    client::{
        SCRAP_OTHER_VERSION_OR_X11_REQUIRED, SCRAP_UBUNTU_HIGHER_REQUIRED, SCRAP_X11_REQUIRED,
    },
    platform::linux::is_x11,
};

lazy_static::lazy_static! {
    static ref CAP_DISPLAY_INFO: RwLock<HashMap<usize, u64>> = RwLock::new(HashMap::new());
    static ref PIPEWIRE_INITIALIZED: RwLock<bool> = RwLock::new(false);
    static ref LOG_SCRAP_COUNT: Mutex<u32> = Mutex::new(0);
    static ref ACTIVE_DISPLAY_COUNT: RwLock<usize> = RwLock::new(0);
}

pub fn init() {
    set_map_err(map_err_scrap);
}

pub(super) fn increment_active_display_count() -> usize {
    let mut count = ACTIVE_DISPLAY_COUNT.write().unwrap();
    *count += 1;
    *count
}

pub(super) fn decrement_active_display_count() -> usize {
    let mut count = ACTIVE_DISPLAY_COUNT.write().unwrap();
    if *count > 0 {
        *count -= 1;
    }
    *count
}

fn map_err_scrap(err: String) -> io::Error {
    // to-do: Handle error better, do not restart server
    if err.starts_with("Did not receive a reply") {
        log::error!("Fatal pipewire error, {}", &err);
        std::process::exit(-1);
    }

    if DISTRO.name.to_uppercase() == "Ubuntu".to_uppercase() {
        if DISTRO.version_id < "21".to_owned() {
            io::Error::new(io::ErrorKind::Other, SCRAP_UBUNTU_HIGHER_REQUIRED)
        } else {
            try_log(&err);
            io::Error::new(io::ErrorKind::Other, err)
        }
    } else {
        try_log(&err);
        if err.contains("org.freedesktop.portal")
            || err.contains("pipewire")
            || err.contains("dbus")
        {
            io::Error::new(io::ErrorKind::Other, SCRAP_OTHER_VERSION_OR_X11_REQUIRED)
        } else {
            io::Error::new(io::ErrorKind::Other, SCRAP_X11_REQUIRED)
        }
    }
}

fn try_log(err: &String) {
    let mut lock_count = LOG_SCRAP_COUNT.lock().unwrap();
    if *lock_count >= 1000000 {
        return;
    }
    if *lock_count % 10000 == 0 {
        log::error!("Failed scrap {}", err);
    }
    *lock_count += 1;
}

struct CapturerPtr(*mut Capturer);

impl Clone for CapturerPtr {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl TraitCapturer for CapturerPtr {
    fn frame<'a>(&'a mut self, timeout: std::time::Duration) -> std::io::Result<Frame<'a>> {
        unsafe { (*self.0).frame(timeout) }
    }
}

struct CapDisplayInfo {
    rects: Vec<((i32, i32), usize, usize)>,
    displays: Vec<DisplayInfo>,
    num: usize,
    primary: usize,
    current: usize,
    capturer: CapturerPtr,
}

#[tokio::main(flavor = "current_thread")]
pub(super) async fn ensure_inited() -> ResultType<()> {
    check_init().await
}

pub(super) fn is_inited() -> Option<Message> {
    if is_x11() {
        None
    } else {
        if CAP_DISPLAY_INFO.read().unwrap().is_empty() {
            let mut msg_out = Message::new();
            let res = MessageBox {
                msgtype: "nook-nocancel-hasclose".to_owned(),
                title: "Wayland".to_owned(),
                text: "Please Select the screen to be shared(Operate on the peer side).".to_owned(),
                link: "".to_owned(),
                ..Default::default()
            };
            msg_out.set_message_box(res);
            Some(msg_out)
        } else {
            None
        }
    }
}

fn get_max_desktop_resolution() -> Option<String> {
    // works with Xwayland
    let output: Output = Command::new(CMD_SH.as_str())
        .arg("-c")
        .arg("xrandr | awk '/current/ { print $8,$9,$10 }'")
        .output()
        .ok()?;

    if output.status.success() {
        let result = String::from_utf8_lossy(&output.stdout);
        Some(result.trim().to_string())
    } else {
        None
    }
}

fn calculate_max_resolution_from_displays(displays: &[Display]) -> (i32, i32) {
    // TODO: this doesn't work in most situations other than sharing all displays
    //  this is because the function only gets called with the displays being shared with pipewire
    //  the xrandr method does work otherwise we could get this correctly using xdg-output-unstable-v1 when xrandr isn't available
    // log::warn!("using incorrect max resolution calculation uinput may not work correctly");
    let (mut max_x, mut max_y) = (0, 0);
    for d in displays {
        let (x, y) = d.origin();
        max_x = max_x.max(x + d.width() as i32);
        max_y = max_y.max(y + d.height() as i32);
    }
    (max_x, max_y)
}

pub(super) async fn check_init() -> ResultType<()> {
    if !is_x11() {
        let mut minx = 0;
        let mut maxx = 0;
        let mut miny = 0;
        let mut maxy = 0;
        let use_uinput = crate::input_service::wayland_use_uinput();

        if CAP_DISPLAY_INFO.read().unwrap().is_empty() {
            let mut lock = CAP_DISPLAY_INFO.write().unwrap();
            if lock.is_empty() {
                // Check if PipeWire is already initialized to prevent duplicate recorder creation
                if *PIPEWIRE_INITIALIZED.read().unwrap() {
                    log::warn!("wayland_diag: Preventing duplicate PipeWire initialization");
                    return Ok(());
                }
                
                let all = Display::all()?;
                *PIPEWIRE_INITIALIZED.write().unwrap() = true;
                let num = all.len();
                let primary = super::display_service::get_primary_2(&all);
                super::display_service::check_update_displays(&all);
                let mut displays = super::display_service::get_sync_displays();
                for display in displays.iter_mut() {
                    display.cursor_embedded = is_cursor_embedded();
                }

                let mut rects: Vec<((i32, i32), usize, usize)> = Vec::new();
                for d in &all {
                    rects.push((d.origin(), d.width(), d.height()));
                }

                log::debug!("#displays={}, primary={}, rects: {:?}, cpus={}/{}", num, primary, rects, num_cpus::get_physical(), num_cpus::get());

                if use_uinput {
                    let (max_width, max_height) = match get_max_desktop_resolution() {
                        Some(result) if !result.is_empty() => {
                            let resolution: Vec<&str> = result.split(" ").collect();
                            if let (Ok(w), Ok(h)) = (
                                resolution[0].parse::<i32>(),
                                resolution.get(2)
                                    .unwrap_or(&"0")
                                    .trim_end_matches(",")
                                    .parse::<i32>()
                            ) {
                                (w, h)
                            } else {
                                calculate_max_resolution_from_displays(&all)
                            }
                        }
                        _ => calculate_max_resolution_from_displays(&all),
                    };

                    minx = 0;
                    maxx = max_width;
                    miny = 0;
                    maxy = max_height;
                }

                // Create individual CapDisplayInfo for each display with its own capturer
                for (idx, display) in all.into_iter().enumerate() {
                    let capturer = Box::into_raw(Box::new(
                        Capturer::new(display).with_context(|| format!("Failed to create capturer for display {}", idx))?,
                    ));
                    let capturer = CapturerPtr(capturer);
                    
                    let cap_display_info = Box::into_raw(Box::new(CapDisplayInfo {
                        rects: rects.clone(),
                        displays: displays.clone(),
                        num,
                        primary,
                        current: idx,
                        capturer,
                    }));
                    
                    lock.insert(idx, cap_display_info as u64);
                }
            }
        }

        if use_uinput {
            if minx != maxx && miny != maxy {
                log::info!(
                    "update mouse resolution: ({}, {}), ({}, {})",
                    minx,
                    maxx,
                    miny,
                    maxy
                );
                allow_err!(input_service::update_mouse_resolution(minx, maxx, miny, maxy).await);
            }
        }
    }
    Ok(())
}

pub(super) async fn get_displays() -> ResultType<Vec<DisplayInfo>> {
    check_init().await?;
    let cap_map = CAP_DISPLAY_INFO.read().unwrap();
    if let Some(addr) = cap_map.values().next() {
        let cap_display_info: *const CapDisplayInfo = *addr as _;
        unsafe {
            let cap_display_info = &*cap_display_info;
            Ok(cap_display_info.displays.clone())
        }
    } else {
        bail!("Failed to get capturer display info");
    }
}

pub(super) fn get_primary() -> ResultType<usize> {
    let cap_map = CAP_DISPLAY_INFO.read().unwrap();
    if let Some(addr) = cap_map.values().next() {
        let cap_display_info: *const CapDisplayInfo = *addr as _;
        unsafe {
            let cap_display_info = &*cap_display_info;
            Ok(cap_display_info.primary)
        }
    } else {
        bail!("Failed to get capturer display info");
    }
}

pub fn clear() {
    if is_x11() {
        return;
    }
    let mut write_lock = CAP_DISPLAY_INFO.write().unwrap();
    for (_, addr) in write_lock.iter() {
        let cap_display_info: *mut CapDisplayInfo = *addr as _;
        unsafe {
            let _box_capturer = Box::from_raw((*cap_display_info).capturer.0);
            let _box_cap_display_info = Box::from_raw(cap_display_info);
        }
    }
    write_lock.clear();
    
    // Reset PipeWire initialization flag to allow recreation on next init
    *PIPEWIRE_INITIALIZED.write().unwrap() = false;
}

pub(super) fn get_capturer_for_display(display_idx: usize) -> ResultType<super::video_service::CapturerInfo> {
    if is_x11() {
        bail!("Do not call this function if not wayland");
    }
    let cap_map = CAP_DISPLAY_INFO.read().unwrap();
    if let Some(addr) = cap_map.get(&display_idx) {
        let cap_display_info: *const CapDisplayInfo = *addr as _;
        unsafe {
            let cap_display_info = &*cap_display_info;
            let rect = cap_display_info.rects[cap_display_info.current];          
            Ok(super::video_service::CapturerInfo {
                origin: rect.0,
                width: rect.1,
                height: rect.2,
                ndisplay: cap_display_info.num,
                current: cap_display_info.current,
                privacy_mode_id: 0,
                _capturer_privacy_mode_id: 0,
                capturer: Box::new(cap_display_info.capturer.clone()),
            })
        }
    } else {
        bail!("Failed to get capturer display info for display {}", display_idx);
    }
}

pub fn common_get_error() -> String {
    if DISTRO.name.to_uppercase() == "Ubuntu".to_uppercase() {
        if DISTRO.version_id < "21".to_owned() {
            return "".to_owned();
        }
    } else {
        // to-do: check other distros
    }
    return "".to_owned();
}
