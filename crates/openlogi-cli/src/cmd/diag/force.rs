//! `openlogi diag force` — ForceSensingButton (`0x19c0`) thresholds.
//!
//! Without `--button` this is a read-only dump; with `--button N --set V` it
//! writes a new current threshold and reads it back. `--mode N` (0-3, with
//! `--button`) writes the level reporting mode instead of, or in addition to,
//! the thresholds. Unlike `dpi` / `smartshift` / `pointer-speed` this is a
//! settings tool: it does not restore what it wrote.

use anyhow::{Context, Result, anyhow, bail};
use clap::{ArgGroup, Args};
use openlogi_hid::write::{ForceButtonConfig, ForceButtonState, ForceLevelMode};

use crate::cmd::diag::select_device;

/// Arguments of `openlogi diag force`: which device, and optionally which
/// button's thresholds or level reporting mode to write.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("writes").multiple(true).args(["set", "mode"])))]
pub struct ForceArgs {
    /// Run against the device whose name contains this string
    /// (case-insensitive) instead of auto-selecting.
    #[arg(long, value_name = "NAME")]
    pub device: Option<String>,

    /// Button index to write (requires `--set` or `--mode`).
    #[arg(long, value_name = "N", requires = "writes")]
    pub button: Option<u8>,

    /// New first threshold for `--button` (lower = lighter press).
    #[arg(long, value_name = "T1", requires = "button")]
    pub set: Option<u16>,

    /// New second threshold (defaults to the device's current one).
    #[arg(long, value_name = "T2", requires = "set")]
    pub set2: Option<u16>,

    /// New level reporting mode for `--button` (0-3; bit 0 = report level 1,
    /// bit 1 = report level 2; Options+ uses 3).
    #[arg(
        long,
        value_name = "N",
        requires = "button",
        value_parser = clap::value_parser!(u8).range(0..=3)
    )]
    pub mode: Option<u8>,
}

/// Dump every force-sensing button of the selected device, or write the
/// thresholds and level reporting mode `args` give for `--button` and print
/// what the device reports back.
///
/// # Errors
///
/// When no device matches, a read or write fails (including on a device
/// without `0x19c0`), or a write is refused before it is sent: a button out
/// of range, one that reports itself not changeable, or thresholds outside
/// its accepted range.
pub async fn run(args: ForceArgs) -> Result<()> {
    // 0x19c0 = ForceSensingButton — auto-skip devices that lack it.
    let (route, name) = select_device(args.device.as_deref(), &[0x19c0]).await?;
    println!("device: {name} ({route})");

    let buttons = openlogi_hid::read_force_buttons(&route)
        .await
        .context("read force buttons")?;
    println!("buttons: {}", buttons.len());

    let Some(button) = args.button else {
        for (index, state) in buttons.iter().enumerate() {
            print_button(index, state);
        }
        return Ok(());
    };
    let state = select_button(button, &buttons)?;
    if let Some(level1) = args.set {
        let config = plan_thresholds(button, state, level1, args.set2)?;
        let after = openlogi_hid::set_force_thresholds(&route, button, config)
            .await
            .context("write force thresholds")?;
        let before = state.thresholds;
        println!(
            "button {button}: level1 {} -> {}, level2 {} -> {} (requested {}, {})",
            before.level1, after.level1, before.level2, after.level2, config.level1, config.level2
        );
    }
    if let Some(mode) = args.mode {
        let after = openlogi_hid::set_force_level_mode(
            &route,
            button,
            ForceLevelMode::from_bits_retain(mode),
        )
        .await
        .with_context(|| format!("write level reporting mode {mode} of button {button}"))?;
        println!(
            "button {button}: level reporting {} -> {} (requested mode={mode})",
            describe_mode(state.level_mode),
            describe_mode(after)
        );
    }
    Ok(())
}

/// The thresholds to write for `--set` / `--set2`, validated against what the
/// button reported.
fn plan_thresholds(
    button: u8,
    state: &ForceButtonState,
    level1: u16,
    level2: Option<u16>,
) -> Result<ForceButtonConfig> {
    if !state.capabilities.changeable {
        bail!("button {button} reports not changeable; refusing to write");
    }
    let config = ForceButtonConfig {
        level1,
        level2: level2.unwrap_or(state.thresholds.level2),
    };
    state
        .capabilities
        .accepts(&config)
        .map_err(|rejection| anyhow!("button {button}: {rejection}"))?;
    Ok(config)
}

fn select_button(button: u8, buttons: &[ForceButtonState]) -> Result<&ForceButtonState> {
    buttons.get(usize::from(button)).ok_or_else(|| {
        anyhow!(
            "button {button} out of range (device reports {} button(s))",
            buttons.len()
        )
    })
}

fn describe_mode(mode: ForceLevelMode) -> String {
    let levels = [
        (ForceLevelMode::REPORT_LEVEL_1, "level1"),
        (ForceLevelMode::REPORT_LEVEL_2, "level2"),
    ]
    .iter()
    .filter(|(flag, _)| mode.contains(*flag))
    .map(|(_, name)| *name)
    .collect::<Vec<_>>();
    let levels = if levels.is_empty() {
        "none".to_owned()
    } else {
        levels.join(" + ")
    };
    format!("mode=0x{:02x} ({levels})", mode.bits())
}

fn print_button(index: usize, state: &ForceButtonState) {
    let ForceButtonState {
        capabilities: caps,
        thresholds: config,
        level_mode,
    } = state;
    println!(
        "button {index}: changeable={} default={} min={} max={}",
        caps.changeable, caps.default, caps.min, caps.max
    );
    println!(
        "  current: level1={} level2={}",
        config.level1, config.level2
    );
    println!("  level reporting: {}", describe_mode(*level_mode));
}

#[cfg(test)]
mod tests {
    use openlogi_hid::write::ForceButtonCapabilities;

    use super::*;

    fn caps() -> ForceButtonCapabilities {
        ForceButtonCapabilities {
            changeable: true,
            default: 300,
            max: 1000,
            min: 100,
        }
    }

    #[test]
    fn describes_each_reported_level() {
        assert_eq!(describe_mode(ForceLevelMode::empty()), "mode=0x00 (none)");
        assert_eq!(
            describe_mode(ForceLevelMode::REPORT_LEVEL_1),
            "mode=0x01 (level1)"
        );
        assert_eq!(
            describe_mode(ForceLevelMode::BOTH),
            "mode=0x03 (level1 + level2)"
        );
    }

    #[test]
    fn rejects_buttons_the_device_does_not_have() {
        let buttons = [state(true), state(true)];
        select_button(1, &buttons).unwrap();
        let err = select_button(2, &buttons).unwrap_err().to_string();
        assert_eq!(err, "button 2 out of range (device reports 2 button(s))");
    }

    fn state(changeable: bool) -> ForceButtonState {
        ForceButtonState {
            capabilities: ForceButtonCapabilities {
                changeable,
                ..caps()
            },
            thresholds: ForceButtonConfig {
                level1: 300,
                level2: 600,
            },
            level_mode: ForceLevelMode::BOTH,
        }
    }

    #[test]
    fn set2_defaults_to_the_current_second_threshold() {
        let config = plan_thresholds(0, &state(true), 200, None).unwrap();
        assert_eq!((config.level1, config.level2), (200, 600));
        let config = plan_thresholds(0, &state(true), 200, Some(700)).unwrap();
        assert_eq!((config.level1, config.level2), (200, 700));
    }

    #[test]
    fn refuses_to_write_an_unchangeable_button() {
        let err = plan_thresholds(1, &state(false), 200, None)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "button 1 reports not changeable; refusing to write");
    }

    #[test]
    fn refuses_thresholds_the_button_does_not_accept() {
        for (level1, level2, message) in [
            (99, 500, "button 0: thresholds must lie within 100..=1000"),
            (
                600,
                500,
                "button 0: the first threshold (600) must not exceed the second (500)",
            ),
        ] {
            let err = plan_thresholds(0, &state(true), level1, Some(level2))
                .unwrap_err()
                .to_string();
            assert_eq!(err, message);
        }
    }
}
