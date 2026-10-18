//! Tests for the presenter configuration pass, against a scripted
//! Spotlight 2: its `0x19c0` force button and `0x19b0` haptics.

use std::assert_matches;
use std::sync::{Arc, Mutex};

use hidpp::feature::haptic_feedback::HapticFeedbackFeature;
use openlogi_core::config::{ForceThreshold, PresenterSettings};

use super::*;
use crate::LOGITECH_VENDOR_ID;
use crate::backend::{BackendError, HotplugStream, NodeInfo, RawWriter};
use crate::channel::scripted::{
    ScriptedRawHidChannel, feature_error, scripted_channel, scripted_node_info,
};
use crate::write::haptic::CACHE_SLOT;

/// Feature index of `0x19c0` on the scripted presenter.
const FORCE_INDEX: u8 = 1;
/// Feature index of `0x19b0` on the scripted presenter.
const HAPTIC_INDEX: u8 = 2;
/// HID++ 2.0 `Busy`.
const BUSY: u8 = 0x08;

fn force(value: u16) -> ForceThreshold {
    ForceThreshold::try_new(value).expect("valid force threshold")
}

/// What the scripted presenter holds and how it misbehaves.
#[derive(Clone)]
struct Device {
    /// Whether it exposes `0x19c0`.
    has_force: bool,
    /// Modes answered to successive level-mode reads; the last one repeats.
    mode_reads: Vec<u8>,
    changeable: bool,
    /// The thresholds the button accepts, `(min, max)`.
    range: (u16, u16),
    /// The stored thresholds, `(touch, press)`.
    thresholds: (u16, u16),
    /// A firmware that acknowledges a threshold write and keeps its own.
    keeps_thresholds: bool,
    /// The `0x19b0` configuration bytes, or `None` without the feature.
    haptics: Option<[u8; 3]>,
    /// Answer this `(feature index, function)` with a `Busy` error.
    fail: Option<(u8, u8)>,
}

impl Default for Device {
    /// A Spotlight 2 as it comes out of the box: first level only, Logitech's
    /// thresholds, haptics armed.
    fn default() -> Self {
        Self {
            has_force: true,
            mode_reads: vec![0x01, 0x03],
            changeable: true,
            range: (93, 933),
            thresholds: (140, 350),
            keeps_thresholds: false,
            haptics: Some([0x01, 60, 0x34]),
            fail: None,
        }
    }
}

/// What the scripted presenter saw.
#[derive(Default)]
struct Seen {
    mode_reads: usize,
    /// The mode byte of each level-mode write.
    mode_writes: Vec<u8>,
    /// `(touch, press)` of each threshold write.
    threshold_writes: Vec<(u16, u16)>,
    haptic_writes: Vec<[u8; 3]>,
}

fn respond(request: &[u8], device: &mut Device, seen: &mut Seen) -> Option<Vec<u8>> {
    if request.len() < 7 || !matches!(request[0], 0x10 | 0x11) {
        return None;
    }
    let function = request[3] >> 4;
    if device.fail == Some((request[2], function)) {
        return Some(feature_error(request, BUSY));
    }
    let mut payload = [0u8; 16];
    match (request[2], function) {
        // Root ping used by Device::new.
        (0x00, 0x01) => payload[0] = 4,
        // Root feature lookup.
        (0x00, 0x00) => {
            payload[0] = match u16::from_be_bytes([request[4], request[5]]) {
                0x19c0 if device.has_force => FORCE_INDEX,
                HapticFeedbackFeature::ID if device.haptics.is_some() => HAPTIC_INDEX,
                _ => 0,
            };
        }
        // getCapabilities: one button.
        (FORCE_INDEX, 0) => payload[0] = 1,
        // getButtonCapabilities: changeable, default, max, min.
        (FORCE_INDEX, 1) => {
            payload[1] = u8::from(device.changeable);
            payload[2..4].copy_from_slice(&140u16.to_be_bytes());
            payload[4..6].copy_from_slice(&device.range.1.to_be_bytes());
            payload[6..8].copy_from_slice(&device.range.0.to_be_bytes());
        }
        // getButtonConfig.
        (FORCE_INDEX, 2) => {
            payload[..2].copy_from_slice(&device.thresholds.0.to_be_bytes());
            payload[2..4].copy_from_slice(&device.thresholds.1.to_be_bytes());
        }
        // setButtonConfig echoes [button, touch, press].
        (FORCE_INDEX, 3) => {
            let touch = u16::from_be_bytes([request[5], request[6]]);
            let press = u16::from_be_bytes([request[7], request[8]]);
            seen.threshold_writes.push((touch, press));
            if !device.keeps_thresholds {
                device.thresholds = (touch, press);
            }
            payload[..5].copy_from_slice(&request[4..9]);
        }
        // getLevelReportingMode.
        (FORCE_INDEX, 5) => {
            payload[0] = device.mode_reads[seen.mode_reads.min(device.mode_reads.len() - 1)];
            seen.mode_reads += 1;
        }
        // setLevelReportingMode echoes [button, mode].
        (FORCE_INDEX, 6) => {
            seen.mode_writes.push(request[5]);
            payload[..2].copy_from_slice(&request[4..6]);
        }
        // Haptics: getCapabilities, getConfiguration, setConfiguration,
        // getHapticStatus.
        (HAPTIC_INDEX, 0) => payload[4..8].copy_from_slice(&u32::MAX.to_be_bytes()),
        (HAPTIC_INDEX, 1) => payload[..3].copy_from_slice(&device.haptics?),
        (HAPTIC_INDEX, 2) => {
            let state = device.haptics.as_mut()?;
            state[..2].copy_from_slice(&request[4..6]);
            seen.haptic_writes.push(*state);
            payload[..3].copy_from_slice(state);
        }
        (HAPTIC_INDEX, 3) => {}
        _ => return None,
    }
    let mut response = vec![0x11, request[1], request[2], request[3]];
    response.extend_from_slice(&payload);
    Some(response)
}

/// A scripted presenter on a live channel, and what it saw.
async fn scripted_presenter(device: Device) -> (Arc<HidppChannel>, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let state = Mutex::new((device, Arc::clone(&seen)));
    let (raw, _handle) = ScriptedRawHidChannel::with_dynamic_responder(move |request| {
        let mut state = state.lock().unwrap();
        let (device, seen) = &mut *state;
        let mut seen = seen.lock().unwrap();
        respond(request, device, &mut seen)
    });
    (scripted_channel(raw).await, seen)
}

async fn apply_with(
    device: Device,
    settings: PresenterSettings,
) -> (Result<PresenterApplied, WriteError>, Arc<Mutex<Seen>>) {
    let (channel, seen) = scripted_presenter(device).await;
    let result = apply_on_channel(&channel, 1, settings).await;
    (result, seen)
}

/// A backend whose one Logitech node opens into `channel`.
struct PresenterBackend(Arc<HidppChannel>);

#[hidpp::async_trait]
impl HidBackend for PresenterBackend {
    async fn enumerate(&self) -> Result<Vec<NodeInfo>, BackendError> {
        self.enumerate_hidpp().await
    }

    async fn enumerate_hidpp(&self) -> Result<Vec<NodeInfo>, BackendError> {
        Ok(vec![scripted_node_info("presenter")])
    }

    async fn open_hidpp(
        &self,
        _node: &NodeInfo,
    ) -> Result<Option<Arc<HidppChannel>>, BackendError> {
        Ok(Some(Arc::clone(&self.0)))
    }

    async fn open_raw_writer(&self, _node: &NodeInfo) -> Result<Box<dyn RawWriter>, BackendError> {
        Err(BackendError::Backend("no raw writer".into()))
    }

    fn watch(&self) -> Result<HotplugStream, BackendError> {
        Ok(Box::new(futures_lite::stream::empty()))
    }
}

/// The route to [`PresenterBackend`]'s node.
fn presenter_route() -> DeviceRoute {
    DeviceRoute::Direct {
        vendor_id: LOGITECH_VENDOR_ID,
        product_id: scripted_node_info("presenter").product_id,
    }
}

/// A backend over a scripted `device`, and what the device saw.
async fn presenter_backend(device: Device) -> (PresenterBackend, Arc<Mutex<Seen>>) {
    let (channel, seen) = scripted_presenter(device).await;
    (PresenterBackend(channel), seen)
}

/// Settings that turn the haptics off, for passes that look at the force
/// configuration only.
fn force_only() -> PresenterSettings {
    PresenterSettings {
        haptics: false,
        ..PresenterSettings::default()
    }
}

#[tokio::test(start_paused = true)]
async fn a_fresh_presenter_is_switched_over_and_given_the_thresholds() {
    let _slot = CACHE_SLOT.lock().await;
    let (result, seen) = apply_with(Device::default(), PresenterSettings::default()).await;
    assert_eq!(
        result.expect("the scripted device answers every call"),
        PresenterApplied {
            level_mode: LevelModeApplied::Written,
            thresholds: ThresholdsApplied::Written,
            haptics: Ok(HapticsApplied::Unchanged),
        }
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.mode_writes, [0x03]);
    assert_eq!(seen.threshold_writes, [(93, 300)]);
}

#[tokio::test(start_paused = true)]
async fn a_configured_presenter_is_left_alone() {
    let device = Device {
        mode_reads: vec![0x03],
        thresholds: (93, 300),
        ..Device::default()
    };
    let (result, seen) = apply_with(device, PresenterSettings::default()).await;
    assert_eq!(
        result.expect("nothing to fix"),
        PresenterApplied {
            level_mode: LevelModeApplied::Unchanged,
            thresholds: ThresholdsApplied::Unchanged,
            haptics: Ok(HapticsApplied::Unchanged),
        }
    );
    let seen = seen.lock().unwrap();
    assert_eq!(
        (
            seen.mode_writes.len(),
            seen.threshold_writes.len(),
            seen.haptic_writes.len()
        ),
        (0, 0, 0)
    );
}

#[tokio::test(start_paused = true)]
async fn a_stale_read_back_is_re_read_before_failing() {
    // Mode 1 before, 1 on the immediate read-back, 3 after the settle.
    let device = Device {
        mode_reads: vec![0x01, 0x01, 0x03],
        ..Device::default()
    };
    let (result, seen) = apply_with(device, force_only()).await;
    let applied = result.expect("the re-read sees the written mode");
    assert_eq!(applied.level_mode, LevelModeApplied::Written);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.mode_writes, [0x03]);
    assert_eq!(seen.mode_reads, 3);
}

#[tokio::test(start_paused = true)]
async fn a_mode_that_never_reads_back_fails_after_one_re_read() {
    let device = Device {
        mode_reads: vec![0x01],
        ..Device::default()
    };
    let (result, seen) = apply_with(device, force_only()).await;
    assert_matches!(
        result,
        Err(WriteError::ForceLevelModeNotApplied {
            requested: 0x03,
            actual: 0x01
        })
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.mode_reads, 3);
    assert!(
        seen.threshold_writes.is_empty(),
        "the pass stops at the mode"
    );
}

#[tokio::test(start_paused = true)]
async fn thresholds_the_firmware_keeps_are_reported_as_not_applied() {
    let device = Device {
        keeps_thresholds: true,
        ..Device::default()
    };
    let (result, seen) = apply_with(device, force_only()).await;
    assert_matches!(
        result,
        Err(WriteError::ForceThresholdsNotApplied {
            requested_touch: 93,
            requested_press: 300,
            actual_touch: 140,
            actual_press: 350
        })
    );
    assert_eq!(seen.lock().unwrap().threshold_writes, [(93, 300)]);
}

#[tokio::test(start_paused = true)]
async fn thresholds_outside_the_device_range_are_not_written() {
    // Narrower than the Spotlight 2: the default touch force of 93 is below it.
    let device = Device {
        range: (100, 933),
        ..Device::default()
    };
    let (result, seen) = apply_with(device, force_only()).await;
    assert_eq!(
        result
            .expect("an unfit threshold is an outcome, not an error")
            .thresholds,
        ThresholdsApplied::OutOfRange { min: 100, max: 933 }
    );
    assert!(seen.lock().unwrap().threshold_writes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn an_inverted_pair_is_not_written() {
    let settings = PresenterSettings {
        touch_force: Some(force(420)),
        press_force: Some(force(168)),
        ..force_only()
    };
    let (result, seen) = apply_with(Device::default(), settings).await;
    assert_eq!(
        result
            .expect("an inverted pair is an outcome, not an error")
            .thresholds,
        ThresholdsApplied::Inverted {
            touch: 420,
            press: 168
        }
    );
    assert!(seen.lock().unwrap().threshold_writes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn unchangeable_thresholds_are_not_written() {
    let device = Device {
        changeable: false,
        ..Device::default()
    };
    let (result, seen) = apply_with(device, force_only()).await;
    assert_eq!(
        result.expect("nothing to write").thresholds,
        ThresholdsApplied::NotChangeable
    );
    assert!(seen.lock().unwrap().threshold_writes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_device_without_the_force_button_is_left_alone() {
    let device = Device {
        has_force: false,
        ..Device::default()
    };
    let (result, seen) = apply_with(device, PresenterSettings::default()).await;
    assert_matches!(
        result,
        Err(WriteError::FeatureUnsupported {
            feature_hex: 0x19c0
        })
    );
    let seen = seen.lock().unwrap();
    assert!(seen.haptic_writes.is_empty());
    assert_eq!(seen.mode_reads, 0);
}

#[tokio::test(start_paused = true)]
async fn a_failed_force_read_ends_the_pass_before_the_haptics() {
    let device = Device {
        haptics: Some([0x00, 60, 0x34]),
        fail: Some((FORCE_INDEX, 5)),
        ..Device::default()
    };
    let (result, seen) = apply_with(device, PresenterSettings::default()).await;
    assert_matches!(
        result,
        Err(WriteError::HidppFeature {
            operation: HidppOperation::ConfigurePresenter,
            feature_hex: 0x19c0,
            ..
        })
    );
    assert!(seen.lock().unwrap().haptic_writes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn disabled_haptics_are_enabled_keeping_the_users_intensity() {
    let _slot = CACHE_SLOT.lock().await;
    let device = Device {
        haptics: Some([0x00, 60, 0x34]),
        ..Device::default()
    };
    let (result, seen) = apply_with(device, PresenterSettings::default()).await;
    assert_eq!(
        result.expect("the force button configures").haptics,
        Ok(HapticsApplied::Enabled)
    );
    assert_eq!(seen.lock().unwrap().haptic_writes, [[0x01, 60, 0x34]]);
}

#[tokio::test(start_paused = true)]
async fn haptics_the_user_turned_off_by_intensity_are_left_alone() {
    let _slot = CACHE_SLOT.lock().await;
    let device = Device {
        haptics: Some([0x00, 0, 0x34]),
        ..Device::default()
    };
    let (result, seen) = apply_with(device, PresenterSettings::default()).await;
    assert_eq!(
        result.expect("the force button configures").haptics,
        Ok(HapticsApplied::Unchanged)
    );
    assert!(seen.lock().unwrap().haptic_writes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn haptics_the_config_turns_off_are_disabled_keeping_the_users_intensity() {
    let _slot = CACHE_SLOT.lock().await;
    let device = Device {
        haptics: Some([0x03, 60, 0x34]),
        ..Device::default()
    };
    let (result, seen) = apply_with(device, force_only()).await;
    assert_eq!(
        result.expect("the force button configures").haptics,
        Ok(HapticsApplied::Disabled)
    );
    assert_eq!(seen.lock().unwrap().haptic_writes, [[0x02, 60, 0x34]]);
}

#[tokio::test(start_paused = true)]
async fn haptics_already_off_are_not_written() {
    let _slot = CACHE_SLOT.lock().await;
    let device = Device {
        haptics: Some([0x00, 60, 0x34]),
        ..Device::default()
    };
    let (result, seen) = apply_with(device, force_only()).await;
    assert_eq!(
        result.expect("the force button configures").haptics,
        Ok(HapticsApplied::Unchanged)
    );
    assert!(seen.lock().unwrap().haptic_writes.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_presenter_without_haptics_reports_them_unsupported() {
    let _slot = CACHE_SLOT.lock().await;
    let device = Device {
        haptics: None,
        ..Device::default()
    };
    let (result, _seen) = apply_with(device, PresenterSettings::default()).await;
    assert_eq!(
        result.expect("the force button configures").haptics,
        Ok(HapticsApplied::Unsupported)
    );
}

#[tokio::test(start_paused = true)]
async fn a_haptics_failure_leaves_the_force_configuration_reported() {
    let _slot = CACHE_SLOT.lock().await;
    let device = Device {
        fail: Some((HAPTIC_INDEX, 1)),
        ..Device::default()
    };
    let (result, _seen) = apply_with(device, PresenterSettings::default()).await;
    let applied = result.expect("a haptics failure is carried, not raised");
    assert_eq!(applied.level_mode, LevelModeApplied::Written);
    assert_eq!(applied.thresholds, ThresholdsApplied::Written);
    assert_matches!(
        applied.haptics,
        Err(WriteError::HidppFeature {
            operation: HidppOperation::ConfigurePresenter,
            feature_hex: HapticFeedbackFeature::ID,
            ..
        })
    );
}

#[tokio::test(start_paused = true)]
async fn thresholds_that_do_not_fit_are_refused_before_the_current_ones_are_read() {
    let device = Device {
        range: (100, 933),
        fail: Some((FORCE_INDEX, 2)),
        ..Device::default()
    };
    let (result, _seen) = apply_with(device, force_only()).await;
    assert_eq!(
        result
            .expect("the refusal precedes the failing getButtonConfig")
            .thresholds,
        ThresholdsApplied::OutOfRange { min: 100, max: 933 }
    );
}

#[tokio::test(start_paused = true)]
async fn an_unknown_mode_bit_survives_the_switch_to_both_levels() {
    let device = Device {
        mode_reads: vec![0x80, 0x83],
        ..Device::default()
    };
    let (result, seen) = apply_with(device, force_only()).await;
    assert_eq!(
        result
            .expect("the read-back shows the written mode")
            .level_mode,
        LevelModeApplied::Written
    );
    assert_eq!(seen.lock().unwrap().mode_writes, [0x83]);
}

#[tokio::test(start_paused = true)]
async fn the_thresholds_writer_returns_what_the_device_reports() {
    let (channel, seen) = scripted_presenter(Device::default()).await;
    let force = open_force(&channel, 1).await.expect("0x19c0 is present");
    let wanted = ForceButtonConfig {
        level1: 200,
        level2: 400,
    };
    assert_eq!(write_thresholds(&force, 0, wanted).await, Ok(wanted));
    assert_eq!(seen.lock().unwrap().threshold_writes, [(200, 400)]);
}

#[tokio::test(start_paused = true)]
async fn the_force_buttons_are_read_with_their_capabilities_thresholds_and_mode() {
    let (backend, _seen) = presenter_backend(Device::default()).await;
    let states = read_force_buttons(&backend, &presenter_route())
        .await
        .expect("the scripted device answers every read");
    assert_eq!(
        states,
        [ForceButtonState {
            capabilities: ForceButtonCapabilities {
                changeable: true,
                default: 140,
                max: 933,
                min: 93,
            },
            thresholds: ForceButtonConfig {
                level1: 140,
                level2: 350,
            },
            level_mode: ForceLevelMode::REPORT_LEVEL_1,
        }]
    );
}

#[tokio::test(start_paused = true)]
async fn the_level_mode_setter_writes_the_mode_it_is_given() {
    let device = Device {
        mode_reads: vec![0x02],
        ..Device::default()
    };
    let (backend, seen) = presenter_backend(device).await;
    let mode = set_force_level_mode(
        &backend,
        &presenter_route(),
        0,
        ForceLevelMode::REPORT_LEVEL_2,
    )
    .await
    .expect("the device reads back the mode");
    assert_eq!(mode, ForceLevelMode::REPORT_LEVEL_2);
    assert_eq!(seen.lock().unwrap().mode_writes, [0x02]);
}

#[tokio::test(start_paused = true)]
async fn the_level_mode_setter_reports_a_mode_the_device_does_not_keep() {
    let device = Device {
        mode_reads: vec![0x01],
        ..Device::default()
    };
    let (backend, _seen) = presenter_backend(device).await;
    let result = set_force_level_mode(&backend, &presenter_route(), 0, ForceLevelMode::BOTH).await;
    assert_matches!(
        result,
        Err(WriteError::ForceLevelModeNotApplied {
            requested: 0x03,
            actual: 0x01
        })
    );
}
