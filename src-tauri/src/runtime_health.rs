//! Recovery policy independent of the WebView event loop. No window calls or
//! blocking IPC may run while this state is locked.
use std::collections::BTreeMap;

pub const LABELS: [&str; 4] = ["mascot", "panel", "mascot-menu", "mascot-notification"];
const RESPONSE_TIMEOUT_MS: u64 = 25_000;
const STARTUP_GRACE_MS: u64 = 35_000;
const HEALTHY_RESET_MS: u64 = 120_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repair {
    Reload,
    Recreate,
}

#[derive(Default)]
pub struct ViewHealth {
    pub instance: String,
    pub application_ready: bool,
    pub native_generation: u64,
    pub retired: Vec<String>,
    pub recovered: bool,
    pub restore_visible: bool,
    pub sequence: u64,
    pub last_ack: u64,
    pub last_js: u64,
    pub last_paint: u64,
    pub grace_until: u64,
    pub visible_since: u64,
    pub was_visible: bool,
    pub requested: bool,
    pub browser_failed: bool,
    pub repairs: u8,
    pub healthy_since: Option<u64>,
}

pub struct RuntimeHealth {
    pub epoch: u64,
    pub interactive: bool,
    pub views: BTreeMap<String, ViewHealth>,
}

impl Default for RuntimeHealth {
    fn default() -> Self {
        Self {
            epoch: 1,
            interactive: true,
            views: LABELS
                .into_iter()
                .map(|label| {
                    (
                        label.to_string(),
                        ViewHealth {
                            grace_until: STARTUP_GRACE_MS,
                            restore_visible: label == "mascot",
                            ..ViewHealth::default()
                        },
                    )
                })
                .collect(),
        }
    }
}

impl RuntimeHealth {
    pub fn session(&mut self, interactive: bool, now: u64, force: bool) -> bool {
        if self.interactive == interactive && !force {
            return false;
        }
        self.interactive = interactive;
        self.epoch += 1;
        for view in self.views.values_mut() {
            view.last_js = now;
            view.last_paint = now;
            view.grace_until = now + STARTUP_GRACE_MS;
            view.healthy_since = None;
        }
        true
    }

    pub fn attach(&mut self, label: &str, instance: &str, now: u64) -> bool {
        let Some(view) = self.views.get_mut(label) else {
            return false;
        };
        if instance.is_empty()
            || instance.len() > 100
            || view.retired.iter().any(|old| old == instance)
        {
            return false;
        }
        if view.instance == instance {
            return true;
        }
        if !view.instance.is_empty() {
            return false;
        }
        view.instance = instance.to_owned();
        view.application_ready = false;
        view.last_js = now;
        view.last_paint = now;
        view.grace_until = now + STARTUP_GRACE_MS;
        if view.recovered {
            self.epoch += 1;
        }
        true
    }

    pub fn ack(
        &mut self,
        label: &str,
        instance: &str,
        epoch: u64,
        sequence: u64,
        painted: bool,
        now: u64,
    ) -> bool {
        if epoch != self.epoch || !self.interactive {
            return false;
        }
        let Some(view) = self.views.get_mut(label) else {
            return false;
        };
        if view.instance != instance || sequence < view.last_ack || sequence > view.sequence {
            return false;
        }
        view.last_ack = sequence;
        view.last_js = now;
        if painted {
            view.last_paint = now;
        }
        true
    }

    pub fn request(&mut self, label: &str, browser_failed: bool) {
        if let Some(view) = self.views.get_mut(label) {
            view.requested = true;
            view.browser_failed |= browser_failed;
        }
    }

    pub fn inspect(&mut self, label: &str, visible: bool, now: u64) -> Option<Repair> {
        if !self.interactive {
            return None;
        }
        let view = self.views.get_mut(label)?;
        if visible && !view.was_visible {
            view.visible_since = now;
            view.last_paint = now;
        }
        view.was_visible = visible;
        let required = label == "mascot"
            || visible
            || view.requested
            || (view.recovered && !view.application_ready);
        let js_failed = now.saturating_sub(view.last_js) > RESPONSE_TIMEOUT_MS;
        let paint_failed = visible
            && now.saturating_sub(view.last_paint.max(view.visible_since)) > RESPONSE_TIMEOUT_MS;
        if !view.requested && !js_failed && !paint_failed && view.application_ready {
            let since = view.healthy_since.get_or_insert(now);
            if !view.instance.is_empty() && now.saturating_sub(*since) >= HEALTHY_RESET_MS {
                view.repairs = 0;
            }
            return None;
        }
        if !required {
            return None;
        }
        view.healthy_since = None;
        if now < view.grace_until || view.repairs >= 2 {
            return None;
        }
        let repair = if view.browser_failed || view.repairs > 0 {
            Repair::Recreate
        } else {
            Repair::Reload
        };
        view.repairs += 1;
        view.recovered = true;
        view.application_ready = false;
        // A hidden notification can be prepared off-screen; only its normal
        // generation/layout handshake may show it again.
        view.restore_visible = visible;
        view.requested = false;
        view.browser_failed = false;
        view.grace_until = now + STARTUP_GRACE_MS;
        view.last_js = now;
        view.last_paint = now;
        if !view.instance.is_empty() {
            view.retired.push(std::mem::take(&mut view.instance));
            if view.retired.len() > 16 {
                view.retired.remove(0);
            }
        }
        Some(repair)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_health_lock_never_consumes_repair_budget_and_unlock_has_grace() {
        let mut h = RuntimeHealth::default();
        h.attach("mascot", "old", 0);
        h.session(false, 1_000, false);
        assert_eq!(h.inspect("mascot", true, 8 * 3_600_000), None);
        h.session(true, 8 * 3_600_000, false);
        assert_eq!(h.inspect("mascot", true, 8 * 3_600_000 + 25_000), None);
        assert_eq!(h.views["mascot"].repairs, 0);
    }

    #[test]
    fn runtime_health_reload_then_recreate_is_bounded_without_responsive_renderer() {
        let mut h = RuntimeHealth::default();
        h.attach("mascot", "old", 0);
        assert_eq!(h.inspect("mascot", false, 36_000), Some(Repair::Reload));
        assert_eq!(h.inspect("mascot", false, 72_000), Some(Repair::Recreate));
        assert_eq!(h.inspect("mascot", false, 9_000_000), None);
        assert!(!h.attach("mascot", "old", 75_000));
        assert!(h.attach("mascot", "new", 75_000));
    }

    #[test]
    fn runtime_health_ignores_old_generation_ack_and_hidden_auxiliary_paint() {
        let mut h = RuntimeHealth::default();
        h.attach("mascot-notification", "card", 0);
        h.views.get_mut("mascot-notification").unwrap().sequence = 1;
        h.session(false, 1, false);
        h.session(true, 2, false);
        assert!(!h.ack("mascot-notification", "card", 1, 1, true, 3));
        assert_eq!(h.inspect("mascot-notification", false, 9_000_000), None);
        assert_eq!(
            h.inspect("mascot-notification", true, 9_000_000),
            Some(Repair::Reload)
        );
    }

    #[test]
    fn runtime_health_browser_failure_recreates_directly_and_repeated_request_is_bounded() {
        let mut h = RuntimeHealth::default();
        h.request("mascot-notification", true);
        assert_eq!(
            h.inspect("mascot-notification", false, 36_000),
            Some(Repair::Recreate)
        );
        h.request("mascot-notification", true);
        assert_eq!(h.inspect("mascot-notification", false, 37_000), None);
        assert_eq!(
            h.inspect("mascot-notification", false, 72_000),
            Some(Repair::Recreate)
        );
        h.request("mascot-notification", true);
        assert_eq!(h.inspect("mascot-notification", false, 900_000), None);
    }

    #[test]
    fn runtime_health_hidden_but_responsive_window_resets_budget_after_sustained_health() {
        let mut h = RuntimeHealth::default();
        h.request("mascot-notification", true);
        assert_eq!(
            h.inspect("mascot-notification", false, 36_000),
            Some(Repair::Recreate)
        );
        assert!(h.attach("mascot-notification", "new-card", 40_000));
        h.views
            .get_mut("mascot-notification")
            .unwrap()
            .application_ready = true;
        for now in (40_000..=165_000).step_by(5_000) {
            h.views.get_mut("mascot-notification").unwrap().sequence += 1;
            let sequence = h.views["mascot-notification"].sequence;
            assert!(h.ack(
                "mascot-notification",
                "new-card",
                h.epoch,
                sequence,
                false,
                now
            ));
            assert_eq!(h.inspect("mascot-notification", false, now), None);
        }
        assert_eq!(h.views["mascot-notification"].repairs, 0);
        h.request("mascot-notification", false);
        assert_eq!(
            h.inspect("mascot-notification", false, 170_000),
            Some(Repair::Reload)
        );
    }

    #[test]
    fn runtime_health_live_javascript_without_completed_app_mount_is_not_healthy() {
        let mut h = RuntimeHealth::default();
        h.attach("mascot", "incomplete", 0);
        h.views.get_mut("mascot").unwrap().sequence = 1;
        assert!(h.ack("mascot", "incomplete", h.epoch, 1, true, 36_000));
        assert_eq!(h.inspect("mascot", true, 36_000), Some(Repair::Reload));
    }
}
