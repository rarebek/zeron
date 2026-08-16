//! Desktop notifications for session transitions.
//!
//! macOS uses Apple's modern UserNotifications framework. Authorization is
//! requested by the UI process, which is also where both local and remote
//! engine state transitions are observed, so remote runs notify on this Mac.

const DISABLE_ENV: &str = "ZERON_DISABLE_NOTIFICATIONS";

/// Ask the desktop notification service for permission once per process.
/// The caller only invokes this when desktop notifications are enabled.
pub fn initialize() {
    #[cfg(target_os = "macos")]
    macos::initialize();
}

/// Post a desktop banner. Failures are intentionally swallowed so a missing
/// notification service can never interrupt a session.
pub fn post(title: &str, body: &str) {
    if std::env::var_os(DISABLE_ENV).is_some() {
        return;
    }
    post_impl(title, body);
}

#[cfg(target_os = "macos")]
fn post_impl(title: &str, body: &str) {
    macos::post(title, body);
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_user_notifications::{
        UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
        UNNotificationSound, UNUserNotificationCenter,
    };

    static INITIALIZED: OnceLock<()> = OnceLock::new();
    static AUTHORIZED: AtomicBool = AtomicBool::new(false);

    pub(super) fn initialize() {
        INITIALIZED.get_or_init(|| {
            let center = UNUserNotificationCenter::currentNotificationCenter();
            let completion: RcBlock<dyn Fn(Bool, *mut NSError)> = RcBlock::new(
                |granted: Bool, error: *mut NSError| {
                AUTHORIZED.store(granted.as_bool() && error.is_null(), Ordering::Release);
                if !error.is_null() {
                    tracing::warn!("macOS notification authorization failed");
                } else if granted.as_bool() {
                    tracing::info!("macOS notifications authorized");
                } else {
                    tracing::info!("macOS notifications denied");
                }
                },
            );
            center.requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert
                    | UNAuthorizationOptions::Sound
                    | UNAuthorizationOptions::Badge,
                &completion,
            );
        });
    }

    pub(super) fn post(title: &str, body: &str) {
        initialize();
        if !AUTHORIZED.load(Ordering::Acquire) {
            tracing::debug!("macOS notification skipped before authorization");
            return;
        }

        let center = UNUserNotificationCenter::currentNotificationCenter();
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setSound(Some(&UNNotificationSound::defaultSound()));
        content.setThreadIdentifier(&NSString::from_str("zeron-sessions"));

        let identifier = format!("zeron-{}", uuid::Uuid::new_v4());
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&identifier),
            &content,
            None,
        );
        let completion: RcBlock<dyn Fn(*mut NSError)> = RcBlock::new(|error: *mut NSError| {
            if !error.is_null() {
                tracing::warn!("macOS notification delivery failed");
            }
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&completion));
    }
}

#[cfg(target_os = "linux")]
fn post_impl(title: &str, body: &str) {
    let (title, body) = (title.to_string(), body.to_string());
    std::thread::spawn(move || {
        let result = std::process::Command::new("notify-send")
            .args(["--app-name=Zeron", "--", &title, &body])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match result {
            Ok(status) if status.success() => {}
            Ok(status) => tracing::debug!(?status, "notify-send failed"),
            Err(err) => tracing::debug!(error = %err, "notify-send unavailable"),
        }
    });
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn post_impl(_title: &str, _body: &str) {}
