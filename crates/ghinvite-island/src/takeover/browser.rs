//! What a browser *displays* for a value the server rendered.
//!
//! Nothing here knows which form is being taken over. These are claims about
//! HTML controls — a `type="number"` input shows nothing for text that is not
//! a number it can hold, a `type="text"` input strips CR and LF, a
//! `<textarea>` normalises its newlines to LF — and each one was checked
//! against Chromium rather than read off the spec, because Chromium is the
//! thing that actually rewrites these values. [`super`] binds them to the link
//! form's fields; this module is where the browser knowledge lives, and where
//! a new control kind is established before any takeover can adopt it
//! (ADR 0001).
//!
//! The distinction matters because the takeover's whole job is to tell "the
//! admin changed this control" apart from "the browser rewrote the server's
//! value". Getting the second one wrong silently repairs a model the server
//! rejected; see [`ControlText::edited`].
//!
//! Everything here is pure and compiles natively, so it is unit-tested without
//! a browser and without reference to any particular form.

/// A text-like control (`<input>`, `<textarea>`, `<select>`) as the island
/// finds it in the live DOM at mount time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControlText {
    /// The DOM `value` property: what the admin sees and what a native POST
    /// would send right now.
    pub value: String,
    /// The value the server rendered — `defaultValue` for an input or
    /// textarea, the `selected` option's value for a select.
    pub default: String,
}

impl ControlText {
    /// A control showing `value` over a server-rendered `default`.
    pub fn new(value: impl Into<String>, default: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            default: default.into(),
        }
    }

    /// The admin changed this control before the island mounted: its value
    /// differs from what the browser shows for the server's, once `sanitiser`
    /// has had its say.
    ///
    /// Each arm has a floor the DOM cannot lift. An admin who *deliberately*
    /// picks the displayed fallback over an unsupported value, or clears a
    /// numeric field the browser had already emptied, leaves the control
    /// identical to how the server rendered it and so reads as untouched. That
    /// is the safe direction: the form stays visibly rejected until a
    /// distinguishable input corrects it, rather than submitting a value the
    /// admin never chose.
    pub(super) fn edited(&self, sanitiser: Sanitiser) -> bool {
        match sanitiser {
            Sanitiser::Plain => self.value != self.default,
            Sanitiser::Line => self.value != self.default.replace(['\n', '\r'], ""),
            Sanitiser::Text => self.value != self.default.replace("\r\n", "\n").replace('\r', "\n"),
            Sanitiser::Number => self.value != self.default && !self.browser_emptied(),
        }
    }

    /// The browser, not the admin, emptied this numeric control: it shows
    /// nothing because the server's value is not a number it can display.
    ///
    /// Reading that as an edit would drop the raw text and its parse blocker,
    /// turning a guardrail the server rejected into "unlimited" or "no
    /// expiration" without anyone asking.
    fn browser_emptied(&self) -> bool {
        self.value.is_empty() && !displayable_number(&self.default)
    }
}

/// The value sanitisation a control applies to the server's value before
/// anyone touches it — decided by the element the server renders, so the
/// caller's field-to-sanitiser mapping must agree with the markup (a test in
/// [`super`] renders the form and checks every control).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sanitiser {
    /// A `<select>` or a checkbox: options and checkbox values are carried
    /// verbatim, so nothing rewrites them.
    Plain,
    /// An `<input type="text">`, which strips CR and LF out of its value.
    ///
    /// A server that preserves a multiline value and rejects it would see that
    /// shortening read as an edit, submitting text the admin never wrote and
    /// retiring the message explaining what was wrong with theirs.
    Line,
    /// A `<textarea>`, which keeps its newlines but normalises them to LF —
    /// again the browser rewriting the server's value rather than an edit.
    Text,
    /// An `<input type="number">`, which shows nothing for text that is not a
    /// number it can display ([`ControlText::browser_emptied`]).
    Number,
}

/// Whether a browser will show `text` in a `type="number"` input.
///
/// Two conditions, both of which Chromium was checked against, since it is the
/// thing that actually empties these inputs:
///
/// 1. HTML's *valid floating-point number* grammar — an optional `-`, then a
///    whole part, a `.` fraction, or both in that order, then an optional `e`
///    exponent. So `.5` is shown (the whole part may be omitted when a
///    fraction follows) while `5.` is not, and neither is a leading `+` nor any
///    surrounding whitespace.
/// 2. The number it denotes is finite. `1e999` and a four-hundred-digit
///    integer are both good grammar and both overflow to infinity, and a
///    browser empties them as readily as it empties `abc`.
///
/// Deliberately **not** a domain parser such as
/// [`ghinvite_ui::link_form::parse_max_uses`]: a server renders plenty of
/// numbers the browser displays happily and the domain still rejects (`0`,
/// `4294967296`), and reading one of those as "the browser emptied this" would
/// silently undo an admin who cleared the field before the island mounted.
fn displayable_number(text: &str) -> bool {
    fn digits(text: &str) -> bool {
        !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
    }

    let body = text.strip_prefix('-').unwrap_or(text);
    let (mantissa, exponent) = match body.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (body, None),
    };
    let mantissa = match mantissa.split_once('.') {
        // A fraction needs digits; the whole part before it may be empty.
        Some((whole, fraction)) => (whole.is_empty() || digits(whole)) && digits(fraction),
        None => digits(mantissa),
    };
    let grammar = mantissa
        && exponent
            .is_none_or(|exponent| digits(exponent.strip_prefix(['+', '-']).unwrap_or(exponent)));
    // Only well-formed text reaches the parser, so Rust's extra spellings
    // (`inf`, `NaN`, `+5`) cannot slip through here.
    grammar && text.parse::<f64>().is_ok_and(f64::is_finite)
}

/// The control that had focus when the island took over, named so it can be
/// found again in the re-rendered form.
///
/// A form's ids are expected to be stable across the takeover (they are the
/// shared components' own), so an id is the primary handle; checkbox groups
/// have no ids and are addressed by the name and value they submit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    /// A control with a stable DOM id (`description`, `permission`, …).
    Id(String),
    /// A checkbox, addressed by the `name`/`value` pair it submits.
    NameValue { name: String, value: String },
}

impl FocusTarget {
    /// The CSS selector that finds this control in the re-rendered form, or
    /// `None` when the identifiers are not plain enough to inline into one.
    /// Focus is a nicety; an unexpected id is a reason to skip restoring it,
    /// never a reason to build a selector that could match something else.
    pub fn selector(&self) -> Option<String> {
        match self {
            Self::Id(id) if is_plain(id) => Some(format!("#{id}")),
            Self::NameValue { name, value } if is_plain(name) && is_plain_value(value) => {
                Some(format!("[name=\"{name}\"][value=\"{value}\"]"))
            }
            _ => None,
        }
    }
}

/// An identifier safe to inline into a selector: an ASCII letter followed by
/// letters, digits, `_` or `-`.
fn is_plain(name: &str) -> bool {
    let mut characters = name.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_alphabetic())
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// An attribute value safe to inline into a quoted selector: the checkbox
/// values these forms submit are numeric ids and `true`.
fn is_plain_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Which way a selection was made, the DOM's `selectionDirection`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaretDirection {
    Forward,
    Backward,
    /// No direction, which is also what an unset or unrecognised
    /// `selectionDirection` means.
    #[default]
    None,
}

impl CaretDirection {
    /// The direction as `setSelectionRange` takes it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Backward => "backward",
            Self::None => "none",
        }
    }

    /// The direction a control reports, defaulting to [`Self::None`] for
    /// anything else — a caret that ends up undirected is a nicety lost, not a
    /// reason to drop the restore.
    pub fn from_dom(direction: Option<&str>) -> Self {
        match direction {
            Some("forward") => Self::Forward,
            Some("backward") => Self::Backward,
            _ => Self::None,
        }
    }
}

/// Where the caret sat in a text control, so typing can continue mid-word
/// after the takeover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub start: u32,
    pub end: u32,
    pub direction: CaretDirection,
}

/// The focus state the island captured before clearing the server's markup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusRestore {
    pub target: FocusTarget,
    /// The caret, for the controls that have one. A `type="number"` input has
    /// no selection API, so this is `None` there.
    pub selection: Option<Selection>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two lists were checked against Chromium: each string was set as a
    /// number input's `value` attribute, and the ones below kept it.
    #[test]
    fn displayable_numbers_are_htmls_finite_valid_floating_point_numbers() {
        for text in [
            "0",
            "00",
            "7",
            "-5",
            "1.0",
            "1e2",
            "1E+2",
            "4294967296",
            ".5",
            "-.5",
            ".0",
            // Enormous and minuscule, but finite as an `f64`.
            "1e308",
            "1e-999",
        ] {
            assert!(displayable_number(text), "{text:?} is displayable");
        }
        // Everything a browser empties out of a number input on its own: bad
        // grammar, or good grammar denoting a number it cannot hold.
        for text in [
            "", "abc", " 7 ", "5.", "1.", ".", "+5", "1e", "e5", "7px", "1.2.3", "1e999", "1E999",
            "1e309", "-1e999",
        ] {
            assert!(!displayable_number(text), "{text:?} is not displayable");
        }
        assert!(!displayable_number(&"9".repeat(400)), "overflowing integer");
    }

    /// A control the admin never touched reads as unedited under every
    /// sanitiser, however the browser chose to display it.
    #[test]
    fn a_value_the_browser_left_alone_is_never_an_edit() {
        for (sanitiser, value) in [
            (Sanitiser::Plain, "pull"),
            (Sanitiser::Line, "one line"),
            (Sanitiser::Text, "one\ntwo"),
            (Sanitiser::Number, "30"),
        ] {
            assert!(
                !ControlText::new(value, value).edited(sanitiser),
                "{value:?} under {sanitiser:?}"
            );
        }
    }

    /// `Plain` carries values verbatim, so any difference is the admin's.
    #[test]
    fn a_plain_control_treats_every_difference_as_an_edit() {
        assert!(ControlText::new("maintain", "pull").edited(Sanitiser::Plain));
        // Even a difference another sanitiser would forgive: a `<select>`
        // never rewrites an option's value.
        assert!(ControlText::new("onetwo", "one\ntwo").edited(Sanitiser::Plain));
    }

    /// An `<input type="text">` strips CR and LF, so the shortened value is
    /// the browser's doing and not an edit — while genuinely retyping it is.
    #[test]
    fn a_line_control_forgives_only_the_strip_the_browser_performs() {
        assert!(!ControlText::new("onetwo", "one\ntwo").edited(Sanitiser::Line));
        assert!(!ControlText::new("onetwo", "one\r\ntwo").edited(Sanitiser::Line));
        assert!(ControlText::new("one two", "one\ntwo").edited(Sanitiser::Line));
    }

    /// A `<textarea>` keeps newlines but normalises CRLF and CR to LF.
    #[test]
    fn a_text_control_forgives_only_line_ending_normalisation() {
        assert!(!ControlText::new("one\ntwo", "one\r\ntwo").edited(Sanitiser::Text));
        assert!(!ControlText::new("one\ntwo", "one\rtwo").edited(Sanitiser::Text));
        assert!(ControlText::new("onetwo", "one\ntwo").edited(Sanitiser::Text));
    }

    /// The asymmetry that #67 turned on: an empty numeric control is an edit
    /// only when the browser could have shown the server's value.
    #[test]
    fn a_number_control_distinguishes_a_cleared_field_from_one_the_browser_emptied() {
        // The browser emptied these itself; the raw text and its blocker stay.
        assert!(!ControlText::new("", "abc").edited(Sanitiser::Number));
        assert!(!ControlText::new("", "1e999").edited(Sanitiser::Number));
        // The browser shows `0` and `4294967296` happily, so an empty field is
        // the admin having cleared it — even though the domain rejects both.
        assert!(ControlText::new("", "0").edited(Sanitiser::Number));
        assert!(ControlText::new("", "4294967296").edited(Sanitiser::Number));
        // Typing over what the browser emptied is still an edit.
        assert!(ControlText::new("5", "abc").edited(Sanitiser::Number));
    }

    #[test]
    fn focus_targets_become_selectors_only_when_their_identifiers_are_plain() {
        assert_eq!(
            FocusTarget::Id("description".into()).selector().as_deref(),
            Some("#description")
        );
        assert_eq!(
            FocusTarget::NameValue {
                name: "repo_ids".into(),
                value: "10".into(),
            }
            .selector()
            .as_deref(),
            Some("[name=\"repo_ids\"][value=\"10\"]")
        );
        assert_eq!(FocusTarget::Id("2fa".into()).selector(), None);
        assert_eq!(FocusTarget::Id("a b".into()).selector(), None);
        assert_eq!(
            FocusTarget::NameValue {
                name: "repo_ids".into(),
                value: "\"]:root".into(),
            }
            .selector(),
            None
        );
    }
}
