//! SMartDeskRemote additions: QuickSupport sessions started from the web portal.
//!
//! The portal serves QuickSupport as `SMartDeskRemote-QuickSupport-sdr<m><code>.exe`,
//! where `<m>` is `n` (normal) or `e` (elevated) and `<code>` is the 6-digit support code
//! the customer entered. On start we optionally relaunch elevated (UAC prompt; declining
//! keeps a normal session) and report this device's ID so the technician can connect.
//! The customer still has to click Accept in the connection window.
//!
//! On macOS the portal serves a zip whose app bundle carries the tag
//! (`SMartDeskRemote-QuickSupport-sdr<m><code>.app`); on Linux it serves the AppImage renamed
//! the same way. Elevation is Windows-only; elsewhere both modes start a normal session.
use hbb_common::{config::Config, log};

/// SMartDeskRemote product version shown in Programs & Features. `crate::VERSION` stays the
/// RustDesk protocol version (1.5.1) because peers compare it to enable features.
pub const PRODUCT_VERSION: &str = "1.0.0.1";

/// Builds are distributed by the SMartDeskRemote server, so the upstream RustDesk update
/// check and auto-update are off unless a user or the admin strategy turns them on.
pub fn apply_defaults() {
    use hbb_common::config::{DEFAULT_LOCAL_SETTINGS, DEFAULT_SETTINGS};
    if let Ok(mut m) = DEFAULT_LOCAL_SETTINGS.write() {
        m.entry("enable-check-update".to_owned()).or_insert_with(|| "N".to_owned());
    }
    if let Ok(mut m) = DEFAULT_SETTINGS.write() {
        m.entry("allow-auto-update".to_owned()).or_insert_with(|| "N".to_owned());
    }
}

/// (elevated, code) parsed from the executable name.
pub fn support_tag(exe: &str) -> Option<(bool, String)> {
    let name = std::path::Path::new(exe).file_name()?.to_string_lossy().to_lowercase();
    let i = name.find("-sdr")? + 4;
    let rest = name.get(i..)?;
    let mut chars = rest.chars();
    let elevated = match chars.next()? {
        'e' => true,
        'n' => false,
        _ => return None,
    };
    let code: String = chars.take_while(|c| c.is_ascii_digit()).collect();
    (code.len() == 6).then_some((elevated, code))
}

/// Called once from the QuickSupport UI process before the UI starts.
///
/// QuickSupport is the one-file portable build: a wrapper unpacks and starts the app, passing
/// its own file name in `RUSTDESK_APPNAME` and full path in `SDR_PORTABLE_PATH`.
#[cfg(windows)]
pub fn on_quick_support_start(exe: &str) {
    let name = std::env::var(crate::common::PORTABLE_APPNAME_RUNTIME_ENV_KEY).unwrap_or_else(|_| exe.to_owned());
    let Some((elevated, code)) = support_tag(&name) else {
        return;
    };
    if elevated && !crate::platform::is_elevated(None).unwrap_or(false) {
        // Relaunch the downloaded file itself through UAC so the elevated copy keeps its
        // session tag. If the customer declines, continue as a normal session.
        let target = std::env::var("SDR_PORTABLE_PATH").unwrap_or_else(|_| exe.to_owned());
        match crate::platform::windows::run_uac(&target, "") {
            Ok(true) => std::process::exit(0),
            _ => log::info!("Elevation declined; continuing without administrator rights"),
        }
    }
    std::thread::spawn(move || register(code));
}

/// The name carrying the support tag: the AppImage file on Linux (`$APPIMAGE`, since the
/// running binary lives in a temporary mount) or the `.app` bundle on macOS.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn support_source(exe: &str) -> String {
    #[cfg(target_os = "linux")]
    if let Ok(p) = std::env::var("APPIMAGE") {
        return p;
    }
    #[cfg(target_os = "macos")]
    if let Some(app) = exe.split('/').find(|c| c.to_lowercase().ends_with(".app")) {
        return app.to_owned();
    }
    exe.to_owned()
}

/// Called once from the UI process of a portable (not installed) macOS / Linux client.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub fn on_quick_support_start(exe: &str) {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| exe.to_owned());
    let Some((elevated, code)) = support_tag(&support_source(&exe)) else {
        return;
    };
    if elevated {
        log::info!("Elevated control is Windows-only; starting a normal support session");
    }
    std::thread::spawn(move || register(code));
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn register(code: String) {
    let Ok(rt) = hbb_common::tokio::runtime::Runtime::new() else {
        return;
    };
    // The ID is assigned by the rendezvous server shortly after start.
    for _ in 0..120 {
        let id = Config::get_id();
        if !id.is_empty() {
            let api = crate::common::get_api_server(
                Config::get_option("api-server"),
                Config::get_option("custom-rendezvous-server"),
            );
            if api.is_empty() {
                return;
            }
            let body = serde_json::json!({ "code": code, "id": id }).to_string();
            let url = format!("{api}/api/v1/support/register");
            match rt.block_on(crate::common::post_request(url, body, "")) {
                Ok(_) => log::info!("Registered with support session"),
                Err(e) => log::error!("Support session registration failed: {e}"),
            }
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::support_tag;

    #[test]
    fn parses_support_tag() {
        assert_eq!(
            support_tag("C:\\Users\\x\\Downloads\\SMartDeskRemote-QuickSupport-sdre123456.exe"),
            Some((true, "123456".to_owned()))
        );
        assert_eq!(
            support_tag("SMartDeskRemote-QuickSupport-sdrn654321 (1).exe"),
            Some((false, "654321".to_owned()))
        );
        assert_eq!(support_tag("SMartDeskRemote-QuickSupport.exe"), None);
        assert_eq!(support_tag("x-sdrn12345.exe"), None);
        assert_eq!(
            support_tag("SMartDeskRemote-QuickSupport-sdrn123456.app"),
            Some((false, "123456".to_owned()))
        );
        assert_eq!(
            support_tag("/home/pat/Downloads/SMartDeskRemote-QuickSupport-sdre654321.AppImage"),
            Some((true, "654321".to_owned()))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_bundle_is_the_source() {
        assert_eq!(
            super::support_source("/private/var/folders/ab/T/AppTranslocation/X/d/SMartDeskRemote-QuickSupport-sdrn123456.app/Contents/MacOS/SMartDeskRemote"),
            "SMartDeskRemote-QuickSupport-sdrn123456.app"
        );
    }
}
