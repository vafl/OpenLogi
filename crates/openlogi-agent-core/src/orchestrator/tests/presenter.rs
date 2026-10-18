//! Which devices the agent configures as presenters.

use super::*;

fn presenter(online: bool) -> AgentDevice {
    presenter_with(online, true)
}

fn presenter_with(online: bool, highlight: bool) -> AgentDevice {
    AgentDevice {
        capabilities: Some(Capabilities {
            buttons: true,
            presenter_highlight: highlight,
            ..Capabilities::default()
        }),
        ..dev("spotlight", 1, online)
    }
}

#[test]
fn an_online_presenter_with_a_route_is_configured() {
    let device = presenter(true);
    assert_eq!(presenter_route(&device, true), device.route.as_ref());
}

#[test]
fn a_device_with_only_mouse_buttons_is_never_asked() {
    let buttons_only = AgentDevice {
        capabilities: Some(Capabilities {
            buttons: true,
            presenter_highlight: false,
            ..Capabilities::default()
        }),
        ..dev("mouse", 1, true)
    };
    assert_eq!(presenter_route(&buttons_only, true), None);
    let unprobed = dev("mouse", 2, true);
    assert_eq!(presenter_route(&unprobed, true), None);
}

#[test]
fn an_offline_or_routeless_presenter_is_not_written() {
    assert_eq!(presenter_route(&presenter(false), true), None);
    let routeless = AgentDevice {
        route: None,
        ..presenter(true)
    };
    assert_eq!(presenter_route(&routeless, true), None);
}

#[test]
fn a_host_without_the_overlay_writes_nothing_to_a_presenter() {
    assert_eq!(presenter_route(&presenter(true), false), None);
}

#[test]
fn presenter_support_found_on_an_online_route_is_applied() {
    assert_eq!(
        presenter_appeared(&[presenter_with(true, false)], &[presenter(true)]),
        vec![0]
    );
    let unprobed = dev("spotlight", 1, true);
    assert_eq!(presenter_appeared(&[unprobed], &[presenter(true)]), vec![0]);
    // Already known, still missing, offline, a first sighting, a device that
    // just came online and a new route are not: a reapply covers those.
    assert!(presenter_appeared(&[presenter(true)], &[presenter(true)]).is_empty());
    assert!(
        presenter_appeared(
            &[presenter_with(true, false)],
            &[presenter_with(true, false)]
        )
        .is_empty()
    );
    assert!(presenter_appeared(&[presenter_with(true, false)], &[presenter(false)]).is_empty());
    assert!(presenter_appeared(&[], &[presenter(true)]).is_empty());
    assert!(presenter_appeared(&[presenter_with(false, false)], &[presenter(true)]).is_empty());
    let other_route = AgentDevice {
        route: dev("spotlight", 2, true).route,
        ..presenter(true)
    };
    assert!(presenter_appeared(&[presenter_with(true, false)], &[other_route]).is_empty());
}
