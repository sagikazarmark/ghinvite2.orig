use dioxus::prelude::*;
use dioxus_field::{
    Binding, ChangeOrigin, FieldContext, FieldControlOptions, FieldMeta, FieldSurface,
    merge_attributes, use_binding, use_field_meta, use_focus_registration,
};
use dioxus_primitives::dioxus_attributes::attributes;
use std::rc::Rc;

use crate::components::field::{
    Field, FieldAppearance, FieldDescription, FieldDescriptionAppearance, FieldError,
    FieldErrorAppearance, FieldLabel,
};

/// daisyUI's colour axis for an input.
///
/// [`InputColor::Default`] emits no class, which is daisyUI's uncoloured input
/// rather than a synonym for [`InputColor::Neutral`].
#[derive(Copy, Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum InputColor {
    #[default]
    Default,
    Neutral,
    Primary,
    Secondary,
    Accent,
    Info,
    Success,
    Warning,
    Error,
}

impl InputColor {
    /// Every value of this axis, in the order the preview renders them.
    pub const ALL: &'static [Self] = &[
        Self::Default,
        Self::Neutral,
        Self::Primary,
        Self::Secondary,
        Self::Accent,
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
            Self::Neutral => "input-neutral",
            Self::Primary => "input-primary",
            Self::Secondary => "input-secondary",
            Self::Accent => "input-accent",
            Self::Info => "input-info",
            Self::Success => "input-success",
            Self::Warning => "input-warning",
            Self::Error => "input-error",
        }
    }
}

/// daisyUI's size axis for an input.
///
/// [`InputSize::Default`] emits no class and renders at the same size as
/// daisyUI's explicit `input-md`.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum InputSize {
    Xs,
    Sm,
    #[default]
    Default,
    Lg,
    Xl,
}

impl InputSize {
    /// Every value of this axis, from smallest to largest.
    pub const ALL: &'static [Self] = &[Self::Xs, Self::Sm, Self::Default, Self::Lg, Self::Xl];

    /// The daisyUI class name for this value, as a complete string literal so
    /// Tailwind's scanner can see it.
    pub const fn class(self) -> &'static str {
        match self {
            Self::Xs => "input-xs",
            Self::Sm => "input-sm",
            Self::Default => "",
            Self::Lg => "input-lg",
            Self::Xl => "input-xl",
        }
    }
}

/// daisyUI's appearance axis for an input.
///
/// [`InputAppearance::Default`] emits no class, which is daisyUI's bordered
/// input rather than a named style.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum InputAppearance {
    #[default]
    Default,
    Ghost,
}

impl InputAppearance {
    /// Every value of this axis, in the order the preview renders them.
    pub const ALL: &'static [Self] = &[Self::Default, Self::Ghost];

    /// The daisyUI class name for this value, as a complete string literal so
    /// Tailwind's scanner can see it.
    pub const fn class(self) -> &'static str {
        match self {
            Self::Default => "",
            Self::Ghost => "input-ghost",
        }
    }
}

/// A native text field styled with daisyUI's `input` class.
///
/// Producer-defined invalidity emits `input-error` when no colour is passed.
/// Binding, metadata, and focus resolve from explicit props, Field Context,
/// then standalone state.
///
/// Classes passed by the caller concatenate with the input's own; every other
/// attribute the caller passes overrides the input's. Event handlers are
/// explicit because Dioxus' extended attributes do not include them.
///
/// When `prefix` or `suffix` is set — `Some(rsx! {})` included — the Component
/// emits daisyUI's wrapper structure instead of the bare element: a `span.input`
/// holding the leading slot, the native `input` as a direct child, and the
/// trailing slot. The typed axes then style the wrapper; caller `attributes`,
/// the Binding contract, and Field metadata keep targeting the native input,
/// and `wrapper_attributes` is the surface for the wrapper's own box (ADR-0031).
#[component]
pub fn Input(
    /// An explicit colour, or `None` to derive error colour from Field metadata.
    #[props(default)]
    color: Option<InputColor>,
    /// daisyUI's size axis.
    #[props(default)]
    size: InputSize,
    /// daisyUI's appearance axis.
    #[props(default)]
    appearance: InputAppearance,
    /// An explicit Field binding, which wins over Field Context.
    binding: Option<Binding<String>>,
    /// Explicit Field metadata, which wins over Field Context.
    meta: Option<FieldMeta>,
    /// The value rendered by the input.
    #[props(default)]
    value: Option<ReadSignal<String>>,
    /// Called with the input's value after user input.
    on_change: Option<EventHandler<String>>,
    /// Called when the native `change` event ends the interaction unit.
    on_commit: Option<EventHandler<()>>,
    /// Called after focus leaves the native input.
    on_focus_exit: Option<EventHandler<()>>,
    /// Whether the native input is required.
    #[props(default)]
    required: Option<bool>,
    /// Whether the native input is disabled.
    #[props(default)]
    disabled: Option<bool>,
    /// A non-interactive adornment rendered inside the control box, before the
    /// native input. Its presence — an empty `Some` included — selects the
    /// wrapper structure; toggle content inside `Some` rather than the option.
    prefix: Option<Element>,
    /// A non-interactive adornment rendered inside the control box, after the
    /// native input. Its presence — an empty `Some` included — selects the
    /// wrapper structure; toggle content inside `Some` rather than the option.
    suffix: Option<Element>,
    /// Attributes for the adorned `span.input` wrapper, which owns the visible
    /// box: width and responsive-size utilities, `join-item`. Classes
    /// concatenate with the wrapper's own exactly as caller `class` does on the
    /// input. Written with `attributes!(span { .. })`; unused without an
    /// adornment.
    #[props(default)]
    wrapper_attributes: Vec<Attribute>,
    #[props(extends = GlobalAttributes)]
    #[props(extends = input)]
    attributes: Vec<Attribute>,
) -> Element {
    let binding = use_binding(binding, String::new());
    let meta = use_field_meta(meta);
    let color = color.map_or_else(
        || {
            if meta.invalid() { "input-error" } else { "" }
        },
        InputColor::class,
    );
    let size = size.class();
    let appearance = appearance.class();
    let binding_value = binding.read;
    let resolved_value = value.unwrap_or(binding_value);
    let mut focus_exit_reported = use_signal(|| false);
    let mut control: Signal<Option<Rc<MountedData>>> = use_signal(|| None);
    let focus_control = use_callback(move |()| {
        if let Some(control) = control() {
            spawn(async move {
                let _ = control.set_focus(true).await;
            });
        }
    });
    use_focus_registration(focus_control);

    // Adorned means the wrapper arm, even for `Some(rsx! {})`: an empty stable
    // slot keeps the tree shape, where collapsing it back to the bare arm would
    // remount the native input mid-session (ADR-0031).
    let adorned = prefix.is_some() || suffix.is_some();
    let base = if adorned {
        // The wrapper owns the box, so the axis classes relocate to it and the
        // native input is styled by daisyUI's `.input input` rules alone.
        Vec::new()
    } else {
        attributes!(input {
            class: "input {color} {size} {appearance}",
        })
    };
    let meta_attributes = meta.attributes_for(
        &FieldControlOptions::new()
            .disabled(disabled)
            .required(required)
            .surface(FieldSurface::NATIVE),
    );
    let merged = merge_attributes(vec![meta_attributes, base, attributes]);
    let change_binding = binding.clone();
    let commit_binding = binding.clone();
    let focus_exit_binding = binding;
    let handle_mounted = move |event: MountedEvent| control.set(Some(event.data()));
    let handle_focus_in = move |_| focus_exit_reported.set(false);
    let handle_input = move |event: FormEvent| {
        let next = event.value();
        change_binding.write(next.clone(), ChangeOrigin::User);
        if let Some(handler) = &on_change {
            handler.call(next);
        }
    };
    let handle_change = move |_| {
        commit_binding.commit();
        if let Some(handler) = &on_commit {
            handler.call(());
        }
    };
    let handle_focus_out = move |_| {
        if focus_exit_reported() {
            return;
        }
        focus_exit_reported.set(true);
        focus_exit_binding.focus_exit();
        if let Some(handler) = &on_focus_exit {
            handler.call(());
        }
    };

    if adorned {
        let wrapper = merge_attributes(vec![
            attributes!(span {
                class: "input {color} {size} {appearance}",
            }),
            wrapper_attributes,
        ]);

        rsx! {
            // Mousedown on the wrapper is only ever a padding or adornment
            // click — the input's own handler stops propagation — so the
            // browser's blur-and-refocus default is cancelled and focus is
            // forwarded to the native input instead, keeping Focus Exit out of
            // mid-edit adornment clicks (ADR-0031).
            span {
                onmousedown: move |event: MouseEvent| {
                    event.prevent_default();
                    focus_control.call(());
                },
                ..wrapper,
                span {
                    class: "empty:hidden",
                    onmousedown: move |event: MouseEvent| event.prevent_default(),
                    {prefix}
                }
                input {
                    value: resolved_value,
                    onmounted: handle_mounted,
                    onfocusin: handle_focus_in,
                    oninput: handle_input,
                    onchange: handle_change,
                    onfocusout: handle_focus_out,
                    onmousedown: move |event: MouseEvent| event.stop_propagation(),
                    ..merged,
                }
                span {
                    class: "empty:hidden",
                    onmousedown: move |event: MouseEvent| event.prevent_default(),
                    {suffix}
                }
            }
        }
    } else {
        rsx! {
            input {
                value: resolved_value,
                onmounted: handle_mounted,
                onfocusin: handle_focus_in,
                oninput: handle_input,
                onchange: handle_change,
                onfocusout: handle_focus_out,
                ..merged,
            }
        }
    }
}

/// The common Field composition for a native text input.
///
/// This Composition sugar intentionally has no children. Use [`Field`] and its
/// Compound parts when content or attributes must land between the parts. Native
/// input attributes and caller classes are forwarded to [`Input`].
#[component]
pub fn InputField(
    /// The context supplied to the input and every Field part.
    #[props(into)]
    context: FieldContext,
    /// The input's visible label.
    label: String,
    /// Supporting text rendered between the input and its error region.
    #[props(default)]
    description: Option<String>,
    /// An explicit colour, or `None` to derive error colour from Field metadata.
    #[props(default)]
    color: Option<InputColor>,
    /// daisyUI's size axis.
    #[props(default)]
    size: InputSize,
    /// daisyUI's appearance axis.
    #[props(default)]
    appearance: InputAppearance,
    /// Whether the surrounding Field emits its default layout utilities.
    #[props(default)]
    field_appearance: FieldAppearance,
    /// Whether supporting text emits its default wrapping utilities.
    #[props(default)]
    description_appearance: FieldDescriptionAppearance,
    /// Whether the error region emits its default semantic colour.
    #[props(default)]
    error_appearance: FieldErrorAppearance,
    /// An explicit Field binding, which wins over `context` for the input.
    binding: Option<Binding<String>>,
    /// Explicit Field metadata, which wins over `context` for the input.
    meta: Option<FieldMeta>,
    /// The value rendered by the input.
    #[props(default)]
    value: Option<ReadSignal<String>>,
    /// Called with the input's value after user input.
    on_change: Option<EventHandler<String>>,
    /// Called when the native `change` event ends the interaction unit.
    on_commit: Option<EventHandler<()>>,
    /// Called after focus leaves the native input.
    on_focus_exit: Option<EventHandler<()>>,
    /// Whether the native input is required.
    #[props(default)]
    required: Option<bool>,
    /// Whether the native input is disabled.
    #[props(default)]
    disabled: Option<bool>,
    /// A non-interactive adornment rendered inside the control box, before the
    /// native input, forwarded to [`Input`].
    prefix: Option<Element>,
    /// A non-interactive adornment rendered inside the control box, after the
    /// native input, forwarded to [`Input`].
    suffix: Option<Element>,
    /// Attributes for the adorned `span.input` wrapper, forwarded to
    /// [`Input`]; unused without an adornment.
    #[props(default)]
    wrapper_attributes: Vec<Attribute>,
    #[props(extends = GlobalAttributes)]
    #[props(extends = input)]
    attributes: Vec<Attribute>,
) -> Element {
    rsx! {
        Field { context, appearance: field_appearance,
            FieldLabel { {label} }
            Input {
                color,
                size,
                appearance,
                binding,
                meta,
                value,
                on_change,
                on_commit,
                on_focus_exit,
                required,
                disabled,
                prefix,
                suffix,
                wrapper_attributes,
                attributes,
            }
            if let Some(description) = description {
                FieldDescription { appearance: description_appearance, {description} }
            }
            FieldError { appearance: error_appearance }
        }
    }
}
