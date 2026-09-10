use dioxus::prelude::*;
use dioxus_primitives::dioxus_attributes::attributes;
use dioxus_primitives::merge_attributes;

/// daisyUI's colour axis for an alert.
///
/// [`AlertColor::Default`] emits no class, which is daisyUI's uncoloured alert.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum AlertColor {
    #[default]
    Default,
    Info,
    Success,
    Warning,
    Error,
}

impl AlertColor {
    /// Every value of this axis, in the order the preview renders them.
    pub const ALL: &'static [Self] = &[
        Self::Default,
        Self::Info,
        Self::Success,
        Self::Warning,
        Self::Error,
    ];

    /// The daisyUI class name for this value, as a complete string literal so
    /// Tailwind's scanner can see it.
    pub const fn class(self) -> &'static str {
        match self {
            Self::Default => "",
            Self::Info => "alert-info",
            Self::Success => "alert-success",
            Self::Warning => "alert-warning",
            Self::Error => "alert-error",
        }
    }
}

/// daisyUI's appearance axis for an alert.
///
/// [`AlertAppearance::Default`] emits no class, which is daisyUI's filled
/// alert rather than a synonym for one of its named styles.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum AlertAppearance {
    #[default]
    Default,
    Outline,
    Dash,
    Soft,
}

impl AlertAppearance {
    /// Every value of this axis, in the order the preview renders them.
    pub const ALL: &'static [Self] = &[Self::Default, Self::Outline, Self::Dash, Self::Soft];

    /// The daisyUI class name for this value, as a complete string literal so
    /// Tailwind's scanner can see it.
    pub const fn class(self) -> &'static str {
        match self {
            Self::Default => "",
            Self::Outline => "alert-outline",
            Self::Dash => "alert-dash",
            Self::Soft => "alert-soft",
        }
    }
}

/// daisyUI's direction axis for an alert.
///
/// The default is explicit so the alert's direction is carried by its class
/// rather than inferred from the absence of one.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum AlertDirection {
    #[default]
    Horizontal,
    Vertical,
}

impl AlertDirection {
    /// Every value of this axis, in the order the preview renders them.
    pub const ALL: &'static [Self] = &[Self::Horizontal, Self::Vertical];

    /// The daisyUI class name for this value, as a complete string literal so
    /// Tailwind's scanner can see it.
    pub const fn class(self) -> &'static str {
        match self {
            Self::Horizontal => "alert-horizontal",
            Self::Vertical => "alert-vertical",
        }
    }
}

/// A box stating something the reader needs to know, carrying daisyUI's
/// `alert` classes.
///
/// The component adds no live-region role. A caller adds one through the global
/// attributes when the alert appears dynamically and should be announced.
///
/// Classes passed by the caller concatenate with the alert's own; every other
/// attribute the caller passes overrides the alert's.
#[component]
pub fn Alert(
    /// daisyUI's colour axis.
    #[props(default)]
    color: AlertColor,
    /// daisyUI's appearance axis.
    #[props(default)]
    appearance: AlertAppearance,
    /// daisyUI's direction axis, which lays the alert's content out in a row
    /// or a column.
    #[props(default)]
    direction: AlertDirection,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
    children: Element,
) -> Element {
    let color = color.class();
    let appearance = appearance.class();
    let direction = direction.class();

    let base = attributes!(div {
        class: "alert {color} {appearance} {direction}",
    });
    let merged = merge_attributes(vec![base, attributes]);

    rsx! {
        div { ..merged, {children} }
    }
}
