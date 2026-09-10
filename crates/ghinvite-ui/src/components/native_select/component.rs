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

/// daisyUI's colour axis for a select.
///
/// [`NativeSelectColor::Default`] emits no class, which is daisyUI's uncoloured
/// select rather than a synonym for [`NativeSelectColor::Neutral`].
#[derive(Copy, Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum NativeSelectColor {
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

impl NativeSelectColor {
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
            Self::Neutral => "select-neutral",
            Self::Primary => "select-primary",
            Self::Secondary => "select-secondary",
            Self::Accent => "select-accent",
            Self::Info => "select-info",
            Self::Success => "select-success",
            Self::Warning => "select-warning",
            Self::Error => "select-error",
        }
    }
}

/// daisyUI's size axis for a select.
///
/// [`NativeSelectSize::Default`] emits no class and renders at the same size as
/// daisyUI's explicit `select-md`.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum NativeSelectSize {
    Xs,
    Sm,
    #[default]
    Default,
    Lg,
    Xl,
}

impl NativeSelectSize {
    /// Every value of this axis, from smallest to largest.
    pub const ALL: &'static [Self] = &[Self::Xs, Self::Sm, Self::Default, Self::Lg, Self::Xl];

    /// The daisyUI class name for this value, as a complete string literal so
    /// Tailwind's scanner can see it.
    pub const fn class(self) -> &'static str {
        match self {
            Self::Xs => "select-xs",
            Self::Sm => "select-sm",
            Self::Default => "",
            Self::Lg => "select-lg",
            Self::Xl => "select-xl",
        }
    }
}

/// daisyUI's appearance axis for a select.
///
/// [`NativeSelectAppearance::Default`] emits no class, which is daisyUI's
/// bordered select rather than a named style.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
#[non_exhaustive]
pub enum NativeSelectAppearance {
    #[default]
    Default,
    Ghost,
}

impl NativeSelectAppearance {
    /// Every value of this axis, in the order the preview renders them.
    pub const ALL: &'static [Self] = &[Self::Default, Self::Ghost];

    /// The daisyUI class name for this value, as a complete string literal so
    /// Tailwind's scanner can see it.
    pub const fn class(self) -> &'static str {
        match self {
            Self::Default => "",
            Self::Ghost => "select-ghost",
        }
    }
}

/// One choice a [`NativeSelect`] renders as a native `<option>`.
///
/// The component owns the mapping between the typed value and the option-value
/// string the native element speaks: an option's string is its position in the
/// list unless [`NativeSelectOption::form_value`] replaces it, and that string
/// is what a native form submits.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeSelectOption<T> {
    value: T,
    label: String,
    form_value: Option<String>,
    disabled: bool,
}

impl<T> NativeSelectOption<T> {
    /// An enabled option carrying `value` and showing `label`.
    pub fn new(value: T, label: impl Into<String>) -> Self {
        Self {
            value,
            label: label.into(),
            form_value: None,
            disabled: false,
        }
    }

    /// The string this option submits as, replacing the positional default.
    pub fn form_value(mut self, form_value: impl Into<String>) -> Self {
        self.form_value = Some(form_value.into());
        self
    }

    /// Whether the native option is disabled.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// A native single-choice select styled with daisyUI's `select` class.
///
/// The component owns the mapping between the typed value and the option-value
/// strings a native `<select>` speaks: options are matched positionally, or by
/// an explicit per-option form value. A `None` value selects the disabled
/// placeholder option, so a `None` write never arrives from the control; pass
/// `placeholder` or `default_value` whenever the value can start as `None`,
/// because a native select with neither displays its first option.
///
/// Producer-defined invalidity emits `select-error` when no colour is passed.
/// Binding, metadata, and focus resolve from explicit props, Field Context,
/// then standalone state.
///
/// Classes passed by the caller concatenate with the select's own; every
/// other attribute the caller passes overrides the select's. Event handlers
/// are explicit because Dioxus' extended attributes do not include them.
#[component]
pub fn NativeSelect<T: Clone + PartialEq + 'static>(
    /// The choices, rendered in order as native `<option>` elements. Each
    /// option's submitted value is its position unless it carries an explicit
    /// form value.
    options: Vec<NativeSelectOption<T>>,
    /// A disabled placeholder option rendered first, selected while no value
    /// is.
    #[props(default)]
    placeholder: Option<String>,
    /// An explicit colour, or `None` to derive error colour from Field metadata.
    #[props(default)]
    color: Option<NativeSelectColor>,
    /// daisyUI's size axis.
    #[props(default)]
    size: NativeSelectSize,
    /// daisyUI's appearance axis.
    #[props(default)]
    appearance: NativeSelectAppearance,
    /// An explicit Field binding, which wins over Field Context.
    binding: Option<Binding<Option<T>>>,
    /// Explicit Field metadata, which wins over Field Context.
    meta: Option<FieldMeta>,
    /// The controlled value of the select. `Some` makes it controlled, and the
    /// signal's own `None` means nothing is selected.
    #[props(default)]
    value: Option<ReadSignal<Option<T>>>,
    /// The value the select starts on when it is not controlled. `None` shows
    /// the placeholder until an option is chosen.
    #[props(default)]
    default_value: Option<T>,
    /// Called with the selected value after user interaction.
    on_change: Option<EventHandler<Option<T>>>,
    /// Called when the native `change` event ends the interaction unit.
    on_commit: Option<EventHandler<()>>,
    /// Called after focus leaves the native select.
    on_focus_exit: Option<EventHandler<()>>,
    /// Whether the native select is required.
    #[props(default)]
    required: Option<bool>,
    /// Whether the native select is disabled.
    #[props(default)]
    disabled: Option<bool>,
    #[props(extends = GlobalAttributes)]
    #[props(extends = select)]
    attributes: Vec<Attribute>,
) -> Element {
    let binding = use_binding(binding, default_value);
    let meta = use_field_meta(meta);
    let color = color.map_or_else(
        || {
            if meta.invalid() { "select-error" } else { "" }
        },
        NativeSelectColor::class,
    );
    let size = size.class();
    let appearance = appearance.class();
    let binding_value = binding.read;
    let resolved = value.unwrap_or(binding_value);
    let resolved_value = resolved();
    // First match wins, by `PartialEq`, so a duplicated value renders as its
    // earliest option.
    let selected_index = resolved_value
        .as_ref()
        .and_then(|value| options.iter().position(|choice| choice.value == *value));
    let emitted: Vec<String> = options
        .iter()
        .enumerate()
        .map(|(index, choice)| {
            choice
                .form_value
                .clone()
                .unwrap_or_else(|| index.to_string())
        })
        .collect();
    // The browser reports a pick as the option's value string and nothing
    // else, so two options speaking the same string would be indistinguishable
    // exactly where it matters, and the empty string is the placeholder's.
    let mut seen = std::collections::HashSet::with_capacity(emitted.len());
    for value in &emitted {
        assert!(
            !value.is_empty(),
            "NativeSelect options must emit non-empty value strings; the empty string is reserved for the placeholder"
        );
        assert!(
            seen.insert(value.as_str()),
            "NativeSelect options must emit unique value strings; {value:?} appears more than once (an explicit form value may collide with another option's positional index)"
        );
    }
    let select_value = selected_index
        .map(|index| emitted[index].clone())
        .unwrap_or_default();
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

    let base = attributes!(select {
        class: "select {color} {size} {appearance}",
    });
    let meta_attributes = meta.attributes_for(
        &FieldControlOptions::new()
            .disabled(disabled)
            .required(required)
            .surface(FieldSurface::NATIVE),
    );
    let merged = merge_attributes(vec![meta_attributes, base, attributes]);
    let parse_options = options.clone();
    let change_binding = binding.clone();
    let commit_binding = binding.clone();
    let focus_exit_binding = binding;

    rsx! {
        select {
            value: select_value,
            onmounted: move |event: MountedEvent| control.set(Some(event.data())),
            onfocusin: move |_| focus_exit_reported.set(false),
            oninput: move |event| {
                // The placeholder's empty string and any unknown value parse to
                // nothing: a `None` write never arrives from the control.
                let raw = event.value();
                let next = parse_options.iter().enumerate().find_map(|(index, choice)| {
                    let matches = match &choice.form_value {
                        Some(form_value) => *form_value == raw,
                        None => index.to_string() == raw,
                    };
                    matches.then(|| choice.value.clone())
                });
                let Some(next) = next else {
                    return;
                };
                let next = Some(next);
                change_binding.write(next.clone(), ChangeOrigin::User);
                if let Some(handler) = &on_change {
                    handler.call(next);
                }
            },
            onchange: move |_| {
                commit_binding.commit();
                if let Some(handler) = &on_commit {
                    handler.call(());
                }
            },
            onfocusout: move |_| {
                if focus_exit_reported() {
                    return;
                }
                focus_exit_reported.set(true);
                focus_exit_binding.focus_exit();
                if let Some(handler) = &on_focus_exit {
                    handler.call(());
                }
            },
            ..merged,
            if let Some(placeholder) = placeholder {
                option {
                    value: "",
                    disabled: true,
                    selected: selected_index.is_none(),
                    {placeholder}
                }
            }
            for (index, choice) in options.iter().enumerate() {
                option {
                    key: "{index}",
                    value: emitted[index].clone(),
                    disabled: choice.disabled,
                    selected: selected_index == Some(index),
                    "{choice.label}"
                }
            }
        }
    }
}

/// The common Field composition for a native select.
///
/// This Composition sugar intentionally has no children. Use [`Field`] and its
/// Compound parts when content or attributes must land between the parts. Native
/// select attributes and caller classes are forwarded to [`NativeSelect`].
#[component]
pub fn NativeSelectField<T: Clone + PartialEq + 'static>(
    /// The context supplied to the select and every Field part.
    #[props(into)]
    context: FieldContext,
    /// The select's visible label.
    label: String,
    /// Supporting text rendered between the select and its error region.
    #[props(default)]
    description: Option<String>,
    /// The choices, rendered in order as native `<option>` elements. Each
    /// option's submitted value is its position unless it carries an explicit
    /// form value.
    options: Vec<NativeSelectOption<T>>,
    /// A disabled placeholder option rendered first, selected while no value
    /// is.
    #[props(default)]
    placeholder: Option<String>,
    /// An explicit colour, or `None` to derive error colour from Field metadata.
    #[props(default)]
    color: Option<NativeSelectColor>,
    /// daisyUI's size axis.
    #[props(default)]
    size: NativeSelectSize,
    /// daisyUI's appearance axis.
    #[props(default)]
    appearance: NativeSelectAppearance,
    /// Whether the surrounding Field emits its default layout utilities.
    #[props(default)]
    field_appearance: FieldAppearance,
    /// Whether supporting text emits its default wrapping utilities.
    #[props(default)]
    description_appearance: FieldDescriptionAppearance,
    /// Whether the error region emits its default semantic colour.
    #[props(default)]
    error_appearance: FieldErrorAppearance,
    /// An explicit Field binding, which wins over `context` for the select.
    binding: Option<Binding<Option<T>>>,
    /// Explicit Field metadata, which wins over `context` for the select.
    meta: Option<FieldMeta>,
    /// The controlled value of the select. `Some` makes it controlled, and the
    /// signal's own `None` means nothing is selected.
    #[props(default)]
    value: Option<ReadSignal<Option<T>>>,
    /// The value the select starts on when it is not controlled. `None` shows
    /// the placeholder until an option is chosen.
    #[props(default)]
    default_value: Option<T>,
    /// Called with the selected value after user interaction.
    on_change: Option<EventHandler<Option<T>>>,
    /// Called when the native `change` event ends the interaction unit.
    on_commit: Option<EventHandler<()>>,
    /// Called after focus leaves the native select.
    on_focus_exit: Option<EventHandler<()>>,
    /// Whether the native select is required.
    #[props(default)]
    required: Option<bool>,
    /// Whether the native select is disabled.
    #[props(default)]
    disabled: Option<bool>,
    #[props(extends = GlobalAttributes)]
    #[props(extends = select)]
    attributes: Vec<Attribute>,
) -> Element {
    rsx! {
        Field { context, appearance: field_appearance,
            FieldLabel { {label} }
            NativeSelect::<T> {
                options,
                placeholder,
                color,
                size,
                appearance,
                binding,
                meta,
                value,
                default_value,
                on_change,
                on_commit,
                on_focus_exit,
                required,
                disabled,
                attributes,
            }
            if let Some(description) = description {
                FieldDescription { appearance: description_appearance, {description} }
            }
            FieldError { appearance: error_appearance }
        }
    }
}
