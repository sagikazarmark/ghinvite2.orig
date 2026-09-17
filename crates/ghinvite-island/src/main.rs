//! Browser entrypoint of the island: mounts [`ghinvite_island::LinkFormIsland`]
//! onto the server-rendered container.
//!
//! Loaded by the page as `<script type="module" src="/assets/ghinvite-island.js">`
//! (a stable loader written by `scripts/build-island.sh` that imports the
//! hashed `dx` bundle). The generated JS initialises the wasm and calls
//! `main`, which:
//!
//! 1. reads the props blob (`<script type="application/json"
//!    id="link-form-props">`) and deserialises it into `LinkFormIslandProps`;
//!    if the block is missing or malformed the island logs to the console and
//!    returns — the server-rendered form stays and works;
//! 2. **reads the live form** and folds it over those props
//!    ([`ghinvite_island::takeover`]). The bundle can land minutes after the
//!    page did, so the props describe the form as the server sent it, not as
//!    the admin has since filled it in. Every control the admin changed is
//!    taken from the DOM; unchanged controls keep the props value, which is
//!    what preserves a raw permission or numeric guardrail the DOM cannot
//!    show. If any control is missing the takeover is abandoned and the
//!    server-rendered form is left alone, because replacing the admin's work
//!    with a half-read snapshot is worse than not enhancing at all;
//! 3. records where focus and the caret sat, then empties
//!    `<div id="link-form-island">`. `dioxus-web`'s non-hydrating mount
//!    **appends** to the root element (verified in the #33 spike: without this
//!    the page ends up with two forms), so the island clears the SSR markup
//!    itself. The first frame the island renders is byte-identical to what it
//!    removed for an untouched form (`tests/parity.rs`), so nothing visibly
//!    changes;
//! 4. marks the root `data-island="mounted"` so headless tests can wait for
//!    `#link-form-island[data-island="mounted"]` — on the container, not the
//!    form, which must stay identical to the server's;
//! 5. launches Dioxus on that root with the adopted props as context, and puts
//!    focus and the caret back once the first frame is in the DOM.
//!
//! Native builds get an empty `main` so `cargo test --workspace` compiles the
//! crate; the browser code is wasm32-only.

#[cfg(not(target_arch = "wasm32"))]
fn main() {}

#[cfg(target_arch = "wasm32")]
fn main() {
    browser::mount();
}

#[cfg(target_arch = "wasm32")]
mod browser {
    // `fields()` comes from the derived `Form` impl on `CreateLinkForm`, so the
    // selectors below are built from the same names the POST uses.
    use dioform::prelude::Form;
    use dioxus::prelude::*;
    use ghinvite_island::LinkFormIsland;
    use ghinvite_island::takeover::{
        CaretDirection, ControlText, FocusRestore, FocusTarget, FormSnapshot, Selection, adopt,
    };
    use ghinvite_ui::link_form::{
        CreateLinkForm, LINK_FORM_ISLAND_PROPS_ID, LINK_FORM_ISLAND_ROOT_ID, LinkFormIslandProps,
        RepositoryChoice,
    };
    use std::cell::Cell;
    use std::rc::Rc;
    use wasm_bindgen::JsCast;

    /// Value of `data-island` on the root once the island has taken over.
    const MOUNTED: &str = "mounted";

    /// What the entrypoint hands the mounted app: the props as adopted from the
    /// live form, plus the focus state to put back afterwards.
    #[derive(Clone)]
    struct Boot {
        props: LinkFormIslandProps,
        focus: Option<FocusRestore>,
    }

    pub fn mount() {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };

        let mut props = match read_props(&document) {
            Ok(props) => props,
            Err(reason) => {
                web_sys::console::error_1(
                    &format!("ghinvite-island: not mounting, {reason}; the server-rendered form stays in place").into(),
                );
                return;
            }
        };

        let Some(root) = document.get_element_by_id(LINK_FORM_ISLAND_ROOT_ID) else {
            web_sys::console::error_1(
                &format!("ghinvite-island: not mounting, no #{LINK_FORM_ISLAND_ROOT_ID}").into(),
            );
            return;
        };

        // Read the admin's work before anything is destroyed. A control the
        // takeover cannot find means the markup is not the form this island
        // knows, so the usable server-rendered form is kept as it is.
        let Some(live) = read_form(&root, &props.repos) else {
            web_sys::console::error_1(
                &"ghinvite-island: not mounting, the server-rendered form is not the one this island renders; it stays in place".into(),
            );
            return;
        };
        props.values = adopt(&props.values, &live);
        let focus = read_focus(&document, &root);

        // See the module docs: dioxus-web appends, so remove the SSR form first.
        root.set_inner_html("");
        if let Err(error) = root.set_attribute("data-island", MOUNTED) {
            web_sys::console::warn_1(&error);
        }

        dioxus::LaunchBuilder::web()
            .with_cfg(dioxus::web::Config::new().rootname(LINK_FORM_ISLAND_ROOT_ID))
            .with_context(Boot { props, focus })
            .launch(Root);
    }

    /// The page's props blob, or why it cannot be used.
    fn read_props(document: &web_sys::Document) -> Result<LinkFormIslandProps, String> {
        let blob = document
            .get_element_by_id(LINK_FORM_ISLAND_PROPS_ID)
            .ok_or_else(|| format!("no #{LINK_FORM_ISLAND_PROPS_ID} props block"))?;
        let text = blob
            .text_content()
            .ok_or_else(|| format!("#{LINK_FORM_ISLAND_PROPS_ID} is empty"))?;
        serde_json::from_str(&text).map_err(|error| format!("props blob is not valid: {error}"))
    }

    /// The form as it stands right now, or `None` if it is not the form this
    /// island renders.
    ///
    /// The controls are addressed by the `name`s the shared model derives, so
    /// the snapshot and the POST keys cannot drift apart. Every control has to
    /// be there, the repository checkboxes included: a scope group offering
    /// anything other than exactly the available repositories, in order, is
    /// markup this island did not render, and reading a subset of it would
    /// quietly narrow the scope the admin had in front of them.
    fn read_form(root: &web_sys::Element, repos: &[RepositoryChoice]) -> Option<FormSnapshot> {
        let fields = CreateLinkForm::fields();
        let checkboxes = query_all(root, &checkbox_selector(fields.repo_ids().field_name()));
        let offered: Option<Vec<u64>> = checkboxes
            .iter()
            .map(|choice| choice.value().parse().ok())
            .collect();
        if offered? != repos.iter().map(|repo| repo.id).collect::<Vec<_>>() {
            return None;
        }
        Some(FormSnapshot {
            description: input(root, fields.description().field_name())?,
            internal_note: textarea(root, fields.internal_note().field_name())?,
            permission: select(root, fields.permission().field_name())?,
            approval_required: checkbox(root, fields.approval_required().field_name())?.checked(),
            max_uses: input(root, fields.max_uses().field_name())?,
            expires_in_days: input(root, fields.expires_in_days().field_name())?,
            checked_repo_ids: repo_ids_where(&checkboxes, |box_| box_.checked()),
            default_repo_ids: repo_ids_where(&checkboxes, |box_| box_.default_checked()),
        })
    }

    fn input(root: &web_sys::Element, name: &str) -> Option<ControlText> {
        let control = query::<web_sys::HtmlInputElement>(root, &named("input", name))?;
        Some(ControlText::new(control.value(), control.default_value()))
    }

    fn textarea(root: &web_sys::Element, name: &str) -> Option<ControlText> {
        let control = query::<web_sys::HtmlTextAreaElement>(root, &named("textarea", name))?;
        Some(ControlText::new(
            control.value(),
            control.default_value().unwrap_or_default(),
        ))
    }

    fn checkbox(root: &web_sys::Element, name: &str) -> Option<web_sys::HtmlInputElement> {
        query(root, &checkbox_selector(name))
    }

    fn checkbox_selector(name: &str) -> String {
        format!("input[type=\"checkbox\"][name=\"{name}\"]")
    }

    /// The select's current value over the option the server marked `selected`
    /// — its `defaultSelected`, which is what a form reset would restore.
    ///
    /// The two fallbacks are HTML's own rules for what a browser displays, not
    /// spare generality: with no `selected` option a select shows its first,
    /// and an option with no `value` submits its text. `PermissionSelect`
    /// happens to render both attributes today, but getting this default wrong
    /// by one string silently changes a guardrail, so the reading matches the
    /// browser rather than the current markup.
    fn select(root: &web_sys::Element, name: &str) -> Option<ControlText> {
        let control = query::<web_sys::HtmlSelectElement>(root, &named("select", name))?;
        let default = control
            .query_selector("option[selected]")
            .ok()
            .flatten()
            .or_else(|| control.query_selector("option").ok().flatten())
            .and_then(|option| option_value(&option))
            .unwrap_or_default();
        Some(ControlText::new(control.value(), default))
    }

    /// An option's submitted value: its `value` attribute, or its text when it
    /// has none.
    fn option_value(option: &web_sys::Element) -> Option<String> {
        option
            .get_attribute("value")
            .or_else(|| option.text_content())
    }

    /// The repository ids of the checkboxes `include` accepts, in DOM order.
    /// Every value has already been parsed by [`read_form`], which refuses the
    /// whole snapshot if one of them is not a repository id.
    fn repo_ids_where(
        checkboxes: &[web_sys::HtmlInputElement],
        include: impl Fn(&web_sys::HtmlInputElement) -> bool,
    ) -> Vec<u64> {
        checkboxes
            .iter()
            .filter(|box_| include(box_))
            .filter_map(|box_| box_.value().parse().ok())
            .collect()
    }

    fn named(element: &str, name: &str) -> String {
        format!("{element}[name=\"{name}\"]")
    }

    fn query<T: JsCast>(root: &web_sys::Element, selector: &str) -> Option<T> {
        root.query_selector(selector)
            .ok()
            .flatten()
            .and_then(|element| element.dyn_into().ok())
    }

    fn query_all(root: &web_sys::Element, selector: &str) -> Vec<web_sys::HtmlInputElement> {
        let Ok(nodes) = root.query_selector_all(selector) else {
            return Vec::new();
        };
        (0..nodes.length())
            .filter_map(|index| nodes.item(index))
            .filter_map(|node| node.dyn_into().ok())
            .collect()
    }

    /// Where focus and the caret sit inside the form, if they do.
    fn read_focus(document: &web_sys::Document, root: &web_sys::Element) -> Option<FocusRestore> {
        let active = document.active_element()?;
        if !root.contains(Some(&active)) {
            return None;
        }
        let target = match active.id() {
            id if !id.is_empty() => FocusTarget::Id(id),
            _ => FocusTarget::NameValue {
                name: active.get_attribute("name")?,
                value: active.get_attribute("value")?,
            },
        };
        target.selector()?;
        Some(FocusRestore {
            selection: Caret::of(&active).and_then(|caret| caret.read()),
            target,
        })
    }

    /// A control with a caret to save and put back.
    ///
    /// Not every control has one: a `type="number"` input has no selection API
    /// at all (reading `selectionStart` on one throws), and neither do the
    /// checkboxes or the select. Naming the two that do keeps that rule in one
    /// place instead of once per direction.
    enum Caret {
        Text(web_sys::HtmlInputElement),
        Area(web_sys::HtmlTextAreaElement),
    }

    impl Caret {
        fn of(element: &web_sys::Element) -> Option<Self> {
            if let Some(area) = element.dyn_ref::<web_sys::HtmlTextAreaElement>() {
                return Some(Self::Area(area.clone()));
            }
            let input = element.dyn_ref::<web_sys::HtmlInputElement>()?;
            (input.type_() == "text").then(|| Self::Text(input.clone()))
        }

        fn read(&self) -> Option<Selection> {
            let (start, end, direction) = match self {
                Self::Text(input) => (
                    input.selection_start(),
                    input.selection_end(),
                    input.selection_direction(),
                ),
                Self::Area(area) => (
                    area.selection_start(),
                    area.selection_end(),
                    area.selection_direction(),
                ),
            };
            Some(Selection {
                start: start.ok()??,
                end: end.ok()??,
                direction: CaretDirection::from_dom(direction.ok().flatten().as_deref()),
            })
        }

        fn write(&self, selection: &Selection) {
            let (start, end, direction) =
                (selection.start, selection.end, selection.direction.as_str());
            let _ = match self {
                Self::Text(input) => {
                    input.set_selection_range_with_direction(start, end, direction)
                }
                Self::Area(area) => area.set_selection_range_with_direction(start, end, direction),
            };
        }
    }

    /// Put focus and the caret back on the re-rendered control. Best effort:
    /// the takeover has already preserved the values, and a control that
    /// cannot be found again is not worth failing the mount over.
    fn restore_focus(restore: &FocusRestore) {
        let Some(selector) = restore.target.selector() else {
            return;
        };
        // Scoped to the island's root, the way the snapshot was read: these
        // selectors describe a control of this form, not of the whole page.
        let Some(control) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id(LINK_FORM_ISLAND_ROOT_ID))
            .and_then(|root| root.query_selector(&selector).ok().flatten())
        else {
            return;
        };
        if let Some(element) = control.dyn_ref::<web_sys::HtmlElement>() {
            let _ = element.focus();
        }
        if let (Some(selection), Some(caret)) = (&restore.selection, Caret::of(&control)) {
            caret.write(selection);
        }
    }

    #[component]
    fn Root() -> Element {
        let boot = use_context::<Boot>();
        let restore = boot.focus.clone();
        // Effects run after the frame reaches the DOM, which is the first
        // moment the re-rendered control exists to focus. The guard keeps a
        // later rerender from stealing focus back from wherever it has moved.
        let restored = use_hook(|| Rc::new(Cell::new(false)));
        use_effect(move || {
            if let Some(restore) = &restore
                && !restored.replace(true)
            {
                restore_focus(restore);
            }
        });
        let props = boot.props;
        rsx! {
            LinkFormIsland {
                csrf_token: props.csrf_token,
                action: props.action,
                values: props.values,
                repos: props.repos,
                now: props.now,
            }
        }
    }
}
