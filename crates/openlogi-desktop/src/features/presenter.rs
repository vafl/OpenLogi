//! The Presenter tab: what a presenter's force-sensing highlight button does,
//! how hard it must be pressed, and whether it vibrates.
//!
//! Five cards. *Highlight button* binds the light touch and the firm press
//! (`ButtonId::PresenterTouch` / `PresenterPress`) to an effect through the
//! ordinary binding commit, binds the short click (`PresenterClick`) to the
//! native left click or nothing, and picks what the mouse pointer does
//! meanwhile. *Appearance*, *Motion*, *Pressure* and *Haptics* edit the device's
//! `[presenter]` config table; the agent applies both to the hardware on
//! reload and on connect, so this view never talks to the device. Only the
//! device's base settings are edited — a presenter has no per-app profiles.
//! A binding a config file holds that the panel does not offer shows as a
//! read-only "Custom" entry instead of reading as another choice.

use gpui::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{
    Icon, IndexPath, Selectable as _,
    input::{InputEvent, InputState},
    select::{SelectEvent, SelectItem, SelectState},
    slider::Slider,
    v_flex,
};
use openlogi_core::binding::{Action, ButtonId, HighlightEffect};
use openlogi_core::color::Rgb;
use openlogi_core::config::{
    ForceThreshold, HighlightPointer, HighlightSensitivity, LaserSize, PresenterSettings,
    SpotlightDim, SpotlightSize,
};

use crate::state::{AppState, PresenterBindings, StateEvent};
use crate::ui::commit_slider::{CommitSlider, SliderRange, SliderUnit};
use crate::ui::components::{
    PanelCard, SELECT_W, Toggle, color_input, color_swatch, control_select, localize_placeholder,
    setting_row, slider_row,
};
use crate::ui::section::section_label;
use crate::ui::theme::{self, ACCENT_BLUE, Palette, Typography as _};

/// An action a picker offers for a highlight-button control.
trait ActionChoice: SelectItem<Value = Self> + Copy + PartialEq + 'static {
    /// The offered choices, in the order they are listed.
    const ALL: &'static [Self];

    fn action(self) -> Action;

    /// The choice `action` is, if the panel offers it.
    fn of(action: &Action) -> Option<Self>;
}

/// What a picker shows for a stored action: a choice it offers, or "Custom"
/// for any other action a config file may hold (which the agent does run).
/// "Custom" is listed only while it is the stored one and does nothing when
/// confirmed, so the panel never rewrites a binding it does not understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry<C> {
    Offered(C),
    Custom,
}

impl<C: ActionChoice> Entry<C> {
    fn of(action: &Action) -> Self {
        C::of(action).map_or(Self::Custom, Self::Offered)
    }

    /// The listing of the offered choices alone.
    fn listing() -> Vec<Self> {
        C::ALL.iter().copied().map(Self::Offered).collect()
    }

    /// The listing of the offered choices, preceded by "Custom".
    fn listing_with_custom() -> Vec<Self> {
        std::iter::once(Self::Custom)
            .chain(C::ALL.iter().copied().map(Self::Offered))
            .collect()
    }
}

impl<C: ActionChoice> SelectItem for Entry<C> {
    type Value = Self;

    fn title(&self) -> SharedString {
        match self {
            Self::Offered(choice) => choice.title(),
            Self::Custom => tr!("presenter.custom"),
        }
    }

    fn value(&self) -> &Self::Value {
        self
    }
}

/// What a highlight-button control can do. "Off" is `Action::None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HighlightChoice {
    LaserDot,
    Spotlight,
    Off,
}

impl ActionChoice for HighlightChoice {
    const ALL: &'static [Self] = &[Self::LaserDot, Self::Spotlight, Self::Off];

    fn action(self) -> Action {
        match self {
            Self::LaserDot => Action::Highlight(HighlightEffect::LaserDot),
            Self::Spotlight => Action::Highlight(HighlightEffect::Spotlight),
            Self::Off => Action::None,
        }
    }

    fn of(action: &Action) -> Option<Self> {
        match action {
            Action::Highlight(HighlightEffect::LaserDot) => Some(Self::LaserDot),
            Action::Highlight(HighlightEffect::Spotlight) => Some(Self::Spotlight),
            Action::None => Some(Self::Off),
            _ => None,
        }
    }
}

impl SelectItem for HighlightChoice {
    type Value = Self;

    fn title(&self) -> SharedString {
        match self {
            Self::LaserDot => tr!("actions.highlight_laser_dot"),
            Self::Spotlight => tr!("actions.highlight_spotlight"),
            Self::Off => tr!("common.off"),
        }
    }

    fn value(&self) -> &Self::Value {
        self
    }
}

/// What the short click can do: stay the firmware's native left click or do
/// nothing. "Left click" is `Action::LeftClick` ("leave it native" for this
/// control), "Off" is `Action::None` (the click is swallowed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickChoice {
    LeftClick,
    Off,
}

impl ActionChoice for ClickChoice {
    const ALL: &'static [Self] = &[Self::LeftClick, Self::Off];

    fn action(self) -> Action {
        match self {
            Self::LeftClick => Action::LeftClick,
            Self::Off => Action::None,
        }
    }

    fn of(action: &Action) -> Option<Self> {
        match action {
            Action::LeftClick => Some(Self::LeftClick),
            Action::None => Some(Self::Off),
            _ => None,
        }
    }
}

impl SelectItem for ClickChoice {
    type Value = Self;

    fn title(&self) -> SharedString {
        match self {
            Self::LeftClick => tr!("actions.left_click"),
            Self::Off => tr!("common.off"),
        }
    }

    fn value(&self) -> &Self::Value {
        self
    }
}

/// A picker of the actions of one highlight-button control.
struct ActionPicker<C: ActionChoice> {
    select: Choices<Entry<C>>,
    /// Whether the listing currently carries the "Custom" entry.
    listing_custom: bool,
}

impl<C: ActionChoice> ActionPicker<C> {
    /// A picker bound to `button`; confirming an offered choice commits its
    /// action.
    fn new(button: ButtonId, window: &mut Window, cx: &mut Context<PresenterPanel>) -> Self {
        let select =
            PresenterPanel::choice_select(&Entry::<C>::listing(), window, cx, move |entry, cx| {
                if let Entry::Offered(choice) = entry {
                    PresenterPanel::commit_binding(button, choice.action(), cx);
                }
            });
        Self {
            select,
            listing_custom: false,
        }
    }

    /// Show the choice `action` is, listing "Custom" only while it is one.
    fn sync(&mut self, action: &Action, window: &mut Window, cx: &mut App) {
        let shown = Entry::<C>::of(action);
        let custom = shown == Entry::Custom;
        let relisted = custom != self.listing_custom;
        if relisted {
            self.listing_custom = custom;
            self.select.update(cx, |select, cx| {
                let items = if custom {
                    Entry::<C>::listing_with_custom()
                } else {
                    Entry::<C>::listing()
                };
                select.set_items(items, window, cx);
            });
        }
        if relisted || self.select.read(cx).selected_value() != Some(&shown) {
            self.select.update(cx, |select, cx| {
                select.set_selected_value(&shown, window, cx);
            });
        }
    }
}

/// The pointer picker's options, in the order they are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PointerChoice(HighlightPointer);

impl PointerChoice {
    const ALL: [Self; 2] = [Self(HighlightPointer::Hide), Self(HighlightPointer::Follow)];
}

impl SelectItem for PointerChoice {
    type Value = Self;

    fn title(&self) -> SharedString {
        match self.0 {
            HighlightPointer::Hide => tr!("presenter.pointer_hidden"),
            HighlightPointer::Follow => tr!("presenter.pointer_follows"),
        }
    }

    fn value(&self) -> &Self::Value {
        self
    }
}

/// Which threshold a slider edits.
#[derive(Clone, Copy)]
enum Force {
    Touch,
    Press,
}

type Choices<C> = Entity<SelectState<Vec<C>>>;

/// Laser colour presets: the default red, then colours that stay visible on
/// both light and dark slides.
const LASER_PALETTE: [Rgb; 6] = [
    PresenterSettings::DEFAULT_LASER_COLOR,
    Rgb::new(0xff, 0x95, 0x00),
    Rgb::new(0x00, 0xff, 0x40),
    Rgb::new(0x00, 0xe5, 0xff),
    Rgb::new(0xff, 0x2d, 0xf0),
    Rgb::WHITE,
];

/// The Presenter tab: the highlight effects, force thresholds, laser and
/// spotlight settings of a presenter, each committed through the shared
/// presenter settings.
pub struct PresenterPanel {
    touch_effect: ActionPicker<HighlightChoice>,
    press_effect: ActionPicker<HighlightChoice>,
    click: ActionPicker<ClickChoice>,
    pointer: Choices<PointerChoice>,
    touch_force: CommitSlider<ForceThreshold>,
    press_force: CommitSlider<ForceThreshold>,
    laser_size: CommitSlider<LaserSize>,
    spotlight_size: CommitSlider<SpotlightSize>,
    spotlight_dim: CommitSlider<SpotlightDim>,
    sensitivity: CommitSlider<HighlightSensitivity>,
    /// The laser colour's hex field.
    laser_hex: Entity<InputState>,
    /// The colour the field was last stamped with; the field is re-stamped
    /// only when the stored colour moves away from it (a swatch, a reload).
    laser_hex_synced: Option<Rgb>,
    /// The last Enter was an invalid value; cleared by the next edit or blur.
    laser_hex_invalid: bool,
    _subscriptions: Vec<Subscription>,
}

impl PresenterPanel {
    /// Build the panel's controls and subscribe to their edits and to the
    /// stored presenter settings.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let touch_effect = ActionPicker::new(ButtonId::PresenterTouch, window, cx);
        let press_effect = ActionPicker::new(ButtonId::PresenterPress, window, cx);
        let click = ActionPicker::new(ButtonId::PresenterClick, window, cx);
        let pointer = Self::choice_select(
            &PointerChoice::ALL,
            window,
            cx,
            |PointerChoice(pointer), cx| AppState::edit_presenter(cx, |it| it.pointer = pointer),
        );
        let laser_size = CommitSlider::new(
            SliderRange::new(LaserSize::MIN, LaserSize::MAX),
            LaserSize::DEFAULT,
            cx,
            |_, size, cx| AppState::edit_presenter(cx, |it| it.laser_size = size),
        );
        let spotlight_size = CommitSlider::new(
            SliderRange::new(SpotlightSize::MIN, SpotlightSize::MAX).step(5.),
            SpotlightSize::DEFAULT,
            cx,
            |_, size, cx| AppState::edit_presenter(cx, |it| it.spotlight_size = size),
        );
        let spotlight_dim = CommitSlider::new(
            SliderRange::new(SpotlightDim::MIN, SpotlightDim::MAX).step(5.),
            SpotlightDim::DEFAULT,
            cx,
            |_, dim, cx| AppState::edit_presenter(cx, |it| it.spotlight_dim = dim),
        );
        let sensitivity = CommitSlider::new(
            SliderRange::new(HighlightSensitivity::MIN, HighlightSensitivity::MAX).step(5.),
            HighlightSensitivity::DEFAULT,
            cx,
            |_, sensitivity, cx| AppState::edit_presenter(cx, |it| it.sensitivity = sensitivity),
        );
        let force_range = SliderRange::new(ForceThreshold::MIN, ForceThreshold::MAX);
        let touch_force = CommitSlider::new(
            force_range,
            PresenterSettings::DEFAULT_TOUCH_FORCE,
            cx,
            |_, force, cx| Self::commit_force(Force::Touch, force, cx),
        );
        let press_force = CommitSlider::new(
            force_range,
            PresenterSettings::DEFAULT_PRESS_FORCE,
            cx,
            |_, force, cx| Self::commit_force(Force::Press, force, cx),
        );
        let laser_hex = cx.new(|cx| InputState::new(window, cx));
        let hex_events = cx.subscribe_in(&laser_hex, window, Self::on_laser_hex_event);
        let repaint = AppState::repaint_on(cx, |event| {
            matches!(
                event,
                StateEvent::BindingsChanged(_) | StateEvent::DeviceConfigChanged(_)
            )
        });
        Self {
            touch_effect,
            press_effect,
            click,
            pointer,
            touch_force,
            press_force,
            laser_size,
            spotlight_size,
            spotlight_dim,
            sensitivity,
            laser_hex,
            laser_hex_synced: None,
            laser_hex_invalid: false,
            _subscriptions: vec![repaint, hex_events],
        }
    }

    /// Commit the hex field on Enter and on blur. An invalid value is never
    /// written: Enter flags it, blur restores the stored colour.
    fn on_laser_hex_event(
        &mut self,
        input: &Entity<InputState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                self.laser_hex_invalid = false;
                cx.notify();
            }
            InputEvent::PressEnter { .. } | InputEvent::Blur => {
                let text = input.read(cx).value();
                if let Some(color) = parse_hex_color(&text) {
                    self.laser_hex_invalid = false;
                    AppState::edit_presenter(cx, |it| it.laser_color = color);
                    let shown = color.to_string();
                    if text.as_ref() != shown {
                        input.update(cx, |input, cx| input.set_value(shown, window, cx));
                    }
                } else if matches!(event, InputEvent::Blur) {
                    // Forget the stamp so the next render restores the stored colour.
                    self.laser_hex_invalid = false;
                    self.laser_hex_synced = None;
                } else {
                    self.laser_hex_invalid = true;
                }
                cx.notify();
            }
            InputEvent::Focus => {}
        }
    }

    /// Keep the hex field on the stored colour and the current locale.
    fn sync_laser_hex(&mut self, color: Rgb, window: &mut Window, cx: &mut Context<Self>) {
        localize_placeholder(
            &self.laser_hex,
            tr!("presenter.laser_color_placeholder"),
            window,
            cx,
        );
        if self.laser_hex_synced != Some(color) {
            self.laser_hex_synced = Some(color);
            self.laser_hex_invalid = false;
            let shown = color.to_string();
            self.laser_hex
                .update(cx, |input, cx| input.set_value(shown, window, cx));
        }
    }

    /// A picker listing `options`; confirming one runs `commit` with it.
    fn choice_select<C: SelectItem<Value = C> + Copy + PartialEq + 'static>(
        options: &[C],
        window: &mut Window,
        cx: &mut Context<Self>,
        commit: impl Fn(C, &mut App) + 'static,
    ) -> Choices<C> {
        let select =
            cx.new(|cx| SelectState::new(options.to_vec(), Some(IndexPath::default()), window, cx));
        cx.subscribe_in(
            &select,
            window,
            move |_, select, event: &SelectEvent<Vec<C>>, _, cx| {
                let SelectEvent::Confirm(_) = event;
                if let Some(choice) = select.read(cx).selected_value().copied() {
                    commit(choice, cx);
                }
            },
        )
        .detach();
        select
    }

    fn commit_binding(button: ButtonId, action: Action, cx: &mut App) {
        AppState::apply(cx, |state| state.commit_presenter_binding(button, action));
    }

    /// Write a released threshold. The other threshold is read from the
    /// stored settings, so a drag never rewrites the control it did not touch.
    /// The edited slider stops at the other one instead of pushing it, so the
    /// pair stays ordered; the commit itself still runs core's
    /// `PresenterSettings::validate`.
    fn commit_force(edited: Force, force: ForceThreshold, cx: &mut App) {
        AppState::edit_presenter(cx, |settings| match edited {
            Force::Touch => {
                settings.touch_force = Some(force.min(settings.effective_press_force()));
            }
            Force::Press => {
                settings.press_force = Some(force.max(settings.effective_touch_force()));
            }
        });
    }

    /// The Highlight button card's rows: what each control does, then the
    /// pointer.
    fn highlight_rows(&self, pal: Palette) -> impl IntoElement {
        v_flex()
            .gap_4()
            .child(setting_row(
                "presenter-touch-effect",
                pal,
                tr!("presenter.light_touch"),
                tr!("presenter.light_touch_description"),
                picker(&self.touch_effect.select),
            ))
            .child(setting_row(
                "presenter-press-effect",
                pal,
                tr!("presenter.firm_press"),
                tr!("presenter.firm_press_description"),
                picker(&self.press_effect.select),
            ))
            .child(setting_row(
                "presenter-click",
                pal,
                tr!("presenter.click"),
                tr!("presenter.click_description"),
                picker(&self.click.select),
            ))
            .child(setting_row(
                "presenter-pointer",
                pal,
                tr!("presenter.pointer"),
                tr!("presenter.pointer_description"),
                picker(&self.pointer),
            ))
    }

    /// The Appearance card's rows: the laser colour, then one slider each for
    /// the laser size and the spotlight's size and dimming.
    fn appearance_rows(&self, pal: Palette, settings: &PresenterSettings) -> impl IntoElement {
        v_flex()
            .gap_4()
            .child(color_row(
                pal,
                settings.laser_color,
                &self.laser_hex,
                self.laser_hex_invalid,
            ))
            .child(value_slider_row(
                "presenter-laser-size",
                pal,
                tr!("presenter.laser_size"),
                tr!("presenter.laser_size_description"),
                tr!("presenter.points", n => self.laser_size.shown(settings.laser_size).to_string()).to_string(),
                &self.laser_size,
            ))
            .child(value_slider_row(
                "presenter-spotlight-size",
                pal,
                tr!("presenter.spotlight_size"),
                tr!("presenter.spotlight_size_description"),
                tr!("presenter.points", n => self.spotlight_size.shown(settings.spotlight_size).to_string()).to_string(),
                &self.spotlight_size,
            ))
            .child(value_slider_row(
                "presenter-spotlight-dim",
                pal,
                tr!("presenter.spotlight_dimming"),
                tr!("presenter.spotlight_dimming_description"),
                format!("{}%", self.spotlight_dim.shown(settings.spotlight_dim)),
                &self.spotlight_dim,
            ))
    }

    /// The Motion card's rows: the sensitivity slider, the precision switch
    /// and the steady-release switch.
    fn motion_rows(&self, pal: Palette, settings: &PresenterSettings) -> impl IntoElement {
        v_flex()
            .gap_4()
            .child(value_slider_row(
                "presenter-sensitivity",
                pal,
                tr!("presenter.sensitivity"),
                tr!("presenter.sensitivity_description"),
                format!("{}%", self.sensitivity.shown(settings.sensitivity)),
                &self.sensitivity,
            ))
            .child(setting_row(
                "presenter-precision",
                pal,
                tr!("presenter.precision"),
                tr!("presenter.precision_description"),
                Toggle::new("presenter-precision-toggle")
                    .selected(settings.precision)
                    .on_change(|precision, _, cx| {
                        let precision = *precision;
                        AppState::edit_presenter(cx, |it| it.precision = precision);
                    }),
            ))
            .child(setting_row(
                "presenter-steady-release",
                pal,
                tr!("presenter.steady_release"),
                tr!("presenter.steady_release_description"),
                Toggle::new("presenter-steady-release-toggle")
                    .selected(settings.steady_release)
                    .on_change(|steady, _, cx| {
                        let steady = *steady;
                        AppState::edit_presenter(cx, |it| it.steady_release = steady);
                    }),
            ))
    }

    /// The Pressure card's rows: both force thresholds.
    fn pressure_rows(&self, pal: Palette, settings: &PresenterSettings) -> impl IntoElement {
        let touch_shown = self.touch_force.shown(settings.effective_touch_force());
        let press_shown = self.press_force.shown(settings.effective_press_force());
        v_flex()
            .gap_4()
            .child(value_slider_row(
                "presenter-touch-force",
                pal,
                tr!("presenter.touch_force"),
                tr!("presenter.touch_force_description"),
                touch_shown.to_string(),
                &self.touch_force,
            ))
            .child(value_slider_row(
                "presenter-press-force",
                pal,
                tr!("presenter.press_force"),
                tr!("presenter.press_force_description"),
                press_shown.to_string(),
                &self.press_force,
            ))
    }

    /// Show `choice` in `select` unless it already does. Guarded: setting the
    /// selection repaints the picker.
    fn sync_select<C: SelectItem<Value = C> + PartialEq + 'static>(
        select: &Choices<C>,
        choice: &C,
        window: &mut Window,
        cx: &mut App,
    ) {
        if select.read(cx).selected_value() != Some(choice) {
            select.update(cx, |select, cx| {
                select.set_selected_value(choice, window, cx);
            });
        }
    }
}

impl Render for PresenterPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = theme::palette(cx);
        let (bindings, settings) = AppState::try_read(cx).map_or_else(
            || (PresenterBindings::default(), PresenterSettings::default()),
            |state| {
                (
                    state.current_presenter_bindings(),
                    state.current_presenter_settings(),
                )
            },
        );
        self.touch_effect.sync(&bindings.touch, window, cx);
        self.press_effect.sync(&bindings.press, window, cx);
        self.click.sync(&bindings.click, window, cx);
        Self::sync_select(&self.pointer, &PointerChoice(settings.pointer), window, cx);
        self.sync_laser_hex(settings.laser_color, window, cx);
        self.laser_size.sync(settings.laser_size, window, cx);
        self.spotlight_size
            .sync(settings.spotlight_size, window, cx);
        self.spotlight_dim.sync(settings.spotlight_dim, window, cx);
        self.sensitivity.sync(settings.sensitivity, window, cx);
        self.touch_force
            .sync(settings.effective_touch_force(), window, cx);
        self.press_force
            .sync(settings.effective_press_force(), window, cx);

        let haptics = setting_row(
            "presenter-haptics",
            pal,
            tr!("presenter.haptic_feedback"),
            tr!("presenter.haptic_feedback_description"),
            Toggle::new("presenter-haptics-toggle")
                .selected(settings.haptics)
                .on_change(|enabled, _, cx| {
                    let enabled = *enabled;
                    AppState::edit_presenter(cx, |it| it.haptics = enabled);
                }),
        );

        v_flex()
            .gap_4()
            .w_full()
            .child(PanelCard::new(
                tr!("presenter.highlight_button"),
                Icon::empty().path("action-icons/mouse-pointer-click.svg"),
                self.highlight_rows(pal),
            ))
            .child(PanelCard::new(
                tr!("presenter.appearance"),
                Icon::empty().path("action-icons/palette.svg"),
                self.appearance_rows(pal, &settings),
            ))
            .child(PanelCard::new(
                tr!("presenter.motion"),
                Icon::empty().path("action-icons/move.svg"),
                self.motion_rows(pal, &settings),
            ))
            .child(PanelCard::new(
                tr!("presenter.pressure"),
                Icon::empty().path("action-icons/gauge.svg"),
                self.pressure_rows(pal, &settings),
            ))
            .child(PanelCard::new(
                tr!("presenter.haptics"),
                Icon::empty().path("action-icons/bolt.svg"),
                haptics,
            ))
    }
}

/// A picker at the width of the longest translated option.
fn picker<D: gpui_component::searchable_list::SearchableListDelegate + 'static>(
    select: &Entity<SelectState<D>>,
) -> impl IntoElement {
    control_select(select)
        .w(px(SELECT_W))
        .menu_width(px(SELECT_W))
}

/// A slider row with its live value in the accent colour, as SmartShift lays
/// out its wheel sensitivity, and the caption every setting carries below the
/// track. `selector` names the row for layout tests.
fn value_slider_row<T: SliderUnit>(
    selector: &'static str,
    pal: Palette,
    label: SharedString,
    description: SharedString,
    value: String,
    slider: &CommitSlider<T>,
) -> impl IntoElement {
    div().debug_selector(move || selector.into()).child(
        slider_row(
            section_label(label, pal),
            div().text_body().text_color(rgb(ACCENT_BLUE)).child(value),
            Slider::new(slider.slider()).horizontal(),
        )
        .child(
            div()
                .text_caption()
                .text_color(pal.text_muted)
                .child(description),
        ),
    )
}

/// Parse the hex field: an optional leading `#` and surrounding whitespace
/// are dropped, then [`Rgb`]'s own parser decides what a colour is.
fn parse_hex_color(text: &str) -> Option<Rgb> {
    let digits = text.trim();
    digits.strip_prefix('#').unwrap_or(digits).parse().ok()
}

/// The laser colour: a hex field beside the label, with the preset swatches
/// as quick picks below it.
fn color_row(
    pal: Palette,
    current: Rgb,
    hex: &Entity<InputState>,
    invalid: bool,
) -> impl IntoElement {
    let swatches: Vec<_> = LASER_PALETTE
        .iter()
        .map(|&color| {
            color_swatch(
                ("presenter-laser-swatch", color.packed()),
                &tr!("presenter.laser_color"),
                color,
                current == color,
                pal,
                move |cx| AppState::edit_presenter(cx, |it| it.laser_color = color),
            )
        })
        .collect();
    v_flex()
        .gap_2()
        .child(setting_row(
            "presenter-laser-color",
            pal,
            tr!("presenter.laser_color"),
            tr!("presenter.laser_color_description"),
            color_input("presenter-laser", pal, current, hex),
        ))
        .child(
            gpui_component::h_flex()
                .gap_2()
                .flex_wrap()
                .children(swatches),
        )
        .when(invalid, |row| {
            row.child(
                div()
                    .debug_selector(|| "presenter-laser-hex-error".into())
                    .text_caption()
                    .text_color(rgb(theme::STATUS_DISABLED))
                    .child(tr!("presenter.laser_color_invalid")),
            )
        })
}

#[cfg(test)]
mod tests;
