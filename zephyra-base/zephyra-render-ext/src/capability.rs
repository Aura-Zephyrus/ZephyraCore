//! Backend capability declaration.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Capability {
    CustomBasic,
    CustomOffscreen,
    AttachmentSwitch,
    Readback,
    DeviceRebuild,
    Screenshot,
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Capability::CustomBasic => write!(f, "custom-basic"),
            Capability::CustomOffscreen => write!(f, "custom-offscreen"),
            Capability::AttachmentSwitch => write!(f, "attachment-switch"),
            Capability::Readback => write!(f, "readback"),
            Capability::DeviceRebuild => write!(f, "device-rebuild"),
            Capability::Screenshot => write!(f, "screenshot"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendCapabilities {
    pub custom_basic: bool,
    pub custom_offscreen: bool,
    pub attachment_switch: bool,
    pub readback: bool,
    pub device_rebuild: bool,
    pub screenshot: bool,
}

impl BackendCapabilities {
    pub fn none() -> Self {
        Self::default()
    }
    pub fn all() -> Self {
        Self {
            custom_basic: true,
            custom_offscreen: true,
            attachment_switch: true,
            readback: true,
            device_rebuild: true,
            screenshot: true,
        }
    }
    pub fn has(&self, cap: Capability) -> bool {
        match cap {
            Capability::CustomBasic => self.custom_basic,
            Capability::CustomOffscreen => self.custom_offscreen,
            Capability::AttachmentSwitch => self.attachment_switch,
            Capability::Readback => self.readback,
            Capability::DeviceRebuild => self.device_rebuild,
            Capability::Screenshot => self.screenshot,
        }
    }
    pub fn with(mut self, cap: Capability, value: bool) -> Self {
        match cap {
            Capability::CustomBasic => self.custom_basic = value,
            Capability::CustomOffscreen => self.custom_offscreen = value,
            Capability::AttachmentSwitch => self.attachment_switch = value,
            Capability::Readback => self.readback = value,
            Capability::DeviceRebuild => self.device_rebuild = value,
            Capability::Screenshot => self.screenshot = value,
        }
        self
    }
    pub fn enabled_list(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.custom_basic {
            v.push("custom-basic");
        }
        if self.custom_offscreen {
            v.push("custom-offscreen");
        }
        if self.attachment_switch {
            v.push("attachment-switch");
        }
        if self.readback {
            v.push("readback");
        }
        if self.device_rebuild {
            v.push("device-rebuild");
        }
        if self.screenshot {
            v.push("screenshot");
        }
        v
    }
    pub fn disabled_list(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if !self.custom_basic {
            v.push("custom-basic");
        }
        if !self.custom_offscreen {
            v.push("custom-offscreen");
        }
        if !self.attachment_switch {
            v.push("attachment-switch");
        }
        if !self.readback {
            v.push("readback");
        }
        if !self.device_rebuild {
            v.push("device-rebuild");
        }
        if !self.screenshot {
            v.push("screenshot");
        }
        v
    }
}

pub trait CapabilityProvider {
    fn capabilities(&self) -> BackendCapabilities;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capability_display() {
        assert_eq!(Capability::CustomBasic.to_string(), "custom-basic");
        assert_eq!(Capability::Screenshot.to_string(), "screenshot");
    }
    #[test]
    fn capabilities_builder() {
        let caps = BackendCapabilities::none()
            .with(Capability::CustomBasic, true)
            .with(Capability::Readback, true);
        assert!(caps.has(Capability::CustomBasic));
        assert!(caps.has(Capability::Readback));
        assert!(!caps.has(Capability::CustomOffscreen));
        assert_eq!(caps.enabled_list(), vec!["custom-basic", "readback"]);
        assert_eq!(caps.disabled_list().len(), 4);
    }
    #[test]
    fn capabilities_all_none() {
        let all = BackendCapabilities::all();
        let none = BackendCapabilities::none();
        for cap in [
            Capability::CustomBasic,
            Capability::CustomOffscreen,
            Capability::AttachmentSwitch,
            Capability::Readback,
            Capability::DeviceRebuild,
            Capability::Screenshot,
        ] {
            assert!(all.has(cap));
            assert!(!none.has(cap));
        }
    }

    #[test]
    fn capability_provider_contract() {
        struct P;
        impl CapabilityProvider for P {
            fn capabilities(&self) -> BackendCapabilities {
                BackendCapabilities::none().with(Capability::CustomBasic, true)
            }
        }
        assert!(P.capabilities().has(Capability::CustomBasic));
    }

    #[test]
    fn capability_display_covers_every_variant() {
        assert_eq!(Capability::CustomBasic.to_string(), "custom-basic");
        assert_eq!(Capability::CustomOffscreen.to_string(), "custom-offscreen");
        assert_eq!(
            Capability::AttachmentSwitch.to_string(),
            "attachment-switch"
        );
        assert_eq!(Capability::Readback.to_string(), "readback");
        assert_eq!(Capability::DeviceRebuild.to_string(), "device-rebuild");
        assert_eq!(Capability::Screenshot.to_string(), "screenshot");
    }

    #[test]
    fn capabilities_with_toggles_every_flag() {
        for cap in [
            Capability::CustomBasic,
            Capability::CustomOffscreen,
            Capability::AttachmentSwitch,
            Capability::Readback,
            Capability::DeviceRebuild,
            Capability::Screenshot,
        ] {
            let on = BackendCapabilities::none().with(cap, true);
            assert!(on.has(cap), "with({cap:?}, true) must enable it");
            let off = BackendCapabilities::all().with(cap, false);
            assert!(!off.has(cap), "with({cap:?}, false) must disable it");
        }
    }

    #[test]
    fn capabilities_lists_reflect_each_flag() {
        let all = BackendCapabilities::all();
        assert_eq!(
            all.enabled_list(),
            vec![
                "custom-basic",
                "custom-offscreen",
                "attachment-switch",
                "readback",
                "device-rebuild",
                "screenshot",
            ]
        );
        assert!(all.disabled_list().is_empty());
        let none = BackendCapabilities::none();
        assert!(none.enabled_list().is_empty());
        assert_eq!(
            none.disabled_list(),
            vec![
                "custom-basic",
                "custom-offscreen",
                "attachment-switch",
                "readback",
                "device-rebuild",
                "screenshot",
            ]
        );
    }
}
