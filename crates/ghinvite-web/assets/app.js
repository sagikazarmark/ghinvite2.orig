/*
 * ghinvite shared client script. Served at /static/app.js and loaded
 * synchronously from every layout's <head>.
 *
 * The Content-Security-Policy (`script-src 'self' 'wasm-unsafe-eval'`) blocks
 * inline <script> blocks, so all page behaviour lives here (or, later, in a
 * Dioxus island). Each section must be safe to run on every page: guard for
 * the absence of the elements it wires.
 */

/* Theme sync: apply the stored light/dark theme before first paint and keep
 * the header toggle in step. Runs while <head> is still parsing, so
 * document.body does not exist yet; the body attribute is set as soon as the
 * parser inserts <body>. */
(function () {
  var key = 'ghinvite-theme';
  var light = 'ghinvite';
  var dark = 'ghinvite-dark';
  var toggleSelector = '[data-theme-toggle]';

  function valid(value) {
    return value === light || value === dark;
  }

  function toggleLabel(theme) {
    return theme === dark ? 'Switch to light theme' : 'Switch to dark theme';
  }

  function nextTheme(theme) {
    return theme === dark ? light : dark;
  }

  var moonPath = 'M20.25 14.15A7.5 7.5 0 0 1 9.85 3.75a8.25 8.25 0 1 0 10.4 10.4Z';
  var sunPath = 'M12 4.5V3m0 18v-1.5M4.5 12H3m18 0h-1.5M6.34 6.34 5.28 5.28m13.44 13.44-1.06-1.06m0-11.32 1.06-1.06M5.28 18.72l1.06-1.06M16.5 12a4.5 4.5 0 1 1-9 0 4.5 4.5 0 0 1 9 0Z';

  function syncToggle(button, theme) {
    var label = toggleLabel(theme);
    var darkActive = theme === dark;
    button.setAttribute('aria-label', label);
    button.setAttribute('title', label);
    button.setAttribute('aria-pressed', darkActive ? 'true' : 'false');
    button.setAttribute('data-current-theme', theme);
    button.setAttribute('data-theme-value', nextTheme(theme));

    var icon = button.querySelector('[data-theme-icon]');
    if (icon) {
      icon.setAttribute('data-current-icon', darkActive ? 'sun' : 'moon');
    }
    var path = button.querySelector('[data-theme-icon-path]');
    if (path) {
      path.setAttribute('d', darkActive ? sunPath : moonPath);
    }
  }

  var stored = light;

  function apply(value) {
    var theme = valid(value) ? value : light;
    document.documentElement.setAttribute('data-theme', theme);
    if (document.body) {
      document.body.setAttribute('data-theme', theme);
    }
    var toggles = document.querySelectorAll(toggleSelector);
    for (var i = 0; i < toggles.length; i += 1) {
      syncToggle(toggles[i], theme);
    }
  }

  try {
    stored = window.localStorage.getItem(key) || light;
  } catch (_) {
    stored = light;
  }
  apply(stored);

  if (!document.body && typeof MutationObserver === 'function') {
    // The server renders <body data-theme="ghinvite">; overwrite it the moment
    // the parser creates the element so a dark-theme user never sees a light
    // flash before DOMContentLoaded.
    var observer = new MutationObserver(function () {
      if (document.body) {
        document.body.setAttribute('data-theme', valid(stored) ? stored : light);
        observer.disconnect();
      }
    });
    observer.observe(document.documentElement, { childList: true });
  }

  document.addEventListener('DOMContentLoaded', function () {
    apply(stored);
  });

  document.addEventListener('click', function (event) {
    var button = event.target.closest(toggleSelector);
    if (!button) {
      return;
    }
    var next = valid(button.getAttribute('data-theme-value')) ? button.getAttribute('data-theme-value') : nextTheme(stored);
    stored = next;
    apply(next);
    try {
      window.localStorage.setItem(key, next);
    } catch (_) {}
  });
})();

/* Repository selection feedback. The POST still checks the entire selection;
 * this read only gives early feedback. Delegated listeners also cover the
 * island replacing the SSR form after the user has already started editing. */
(function () {
  var selector = 'form[data-repository-authority] input[type="checkbox"][name="repo_ids"]';
  var observations = new WeakMap();

  function display(box, observation, message) {
    if (observations.get(box) !== observation || !box.isConnected) return;
    box.setCustomValidity(message);
    box.setAttribute('aria-invalid', message ? 'true' : 'false');
    var row = box.closest('.repo-choice-row') || box.parentElement;
    var status = row.querySelector('[data-repository-check]');
    if (!status) {
      status = document.createElement('span');
      status.setAttribute('data-repository-check', '');
      status.setAttribute('role', 'status');
      status.className = 'text-sm';
      row.appendChild(status);
    }
    status.textContent = box.checked ? (message || 'Access checked') : '';
  }

  async function check(box) {
    var previous = observations.get(box);
    if (previous) previous.controller.abort();
    var observation = { controller: new AbortController(), allowed: false };
    observations.set(box, observation);
    if (!box.checked) {
      display(box, observation, '');
      return;
    }
    display(box, observation, 'Checking access…');
    var timeout = setTimeout(function () { observation.controller.abort(); }, 10000);
    try {
      var url = new URL(box.form.getAttribute('data-repository-authority'), window.location.href);
      if (url.origin !== window.location.origin) throw new Error('Invalid selection endpoint');
      url.searchParams.set('repo_id', box.value);
      var response = await fetch(url, {
        credentials: 'same-origin', cache: 'no-store', redirect: 'error',
        headers: { Accept: 'application/json' }, signal: observation.controller.signal
      });
      if (observations.get(box) !== observation || !box.checked || !box.isConnected) return;
      if (response.ok && (await response.json()).allowed === true) {
        observation.allowed = true;
        display(box, observation, '');
      } else {
        display(box, observation, response.status === 404
          ? 'You cannot select this repository.'
          : 'Access could not be checked. Deselect and select this repository to retry.');
      }
    } catch (_) {
      display(box, observation, 'Access could not be checked. Deselect and select this repository to retry.');
    } finally {
      clearTimeout(timeout);
    }
  }

  document.addEventListener('change', function (event) {
    if (event.target.matches(selector)) check(event.target);
  });
  document.addEventListener('submit', function (event) {
    var form = event.target;
    if (!form.matches('form[data-repository-authority]')) return;
    // These native POSTs only redisplay the form; the server never dispatches
    // creation for them. They must remain usable while authority is unavailable.
    if (event.submitter && ['apply_all', 'reload_repos'].includes(event.submitter.name)) return;
    var blocked = Array.from(form.querySelectorAll('input[name="repo_ids"]:checked')).find(function (box) {
      var observation = observations.get(box);
      if (!observation) check(box);
      return !observation || !observation.allowed;
    });
    if (blocked) {
      event.preventDefault();
      event.stopImmediatePropagation();
      blocked.reportValidity();
    }
  }, true);

  function initialize() {
    document.querySelectorAll(selector).forEach(function (box) {
      if (box.checked && !observations.has(box)) check(box);
    });
  }
  document.addEventListener('DOMContentLoaded', function () {
    initialize();
    new MutationObserver(initialize).observe(document.body, { childList: true, subtree: true });
  });
})();



/* Invitation-code shortcut (home page, signed in): open /i#secret from the
 * code input. Uses delegated listeners and null-checks every lookup so it is
 * inert on pages without the hooks. */
(function () {
  document.addEventListener('click', function (event) {
    var button = event.target.closest('[data-open-invitation-code]');
    if (!button) return;
    openInvitationCode();
  });
  document.addEventListener('keydown', function (event) {
    if (event.key !== 'Enter' || !event.target.matches('[data-invitation-code-input]')) return;
    event.preventDefault();
    openInvitationCode();
  });
  function openInvitationCode() {
    var input = document.querySelector('[data-invitation-code-input]');
    var message = document.querySelector('[data-invitation-code-message]');
    if (!input) return;
    var code = input.value;
    if (!/^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$/.test(code)) {
      if (message) message.textContent = 'Enter a valid invitation code.';
      input.focus();
      return;
    }
    if (message) message.textContent = '';
    input.value = '';
    window.location.href = '/i#' + code;
  }
})();

/* Read-only organization refresh: native POST remains the fallback. */
(function () {
  document.addEventListener('submit', async function (event) {
    var form = event.target;
    if (!form.matches('form[data-organization-refresh]') || !window.fetch) return;
    event.preventDefault();
    var button = form.querySelector('button[type="submit"]');
    var status = form.querySelector('[data-refresh-status]');
    if (button.disabled) return;
    button.disabled = true;
    status.textContent = 'Requesting organization delivery updates…';
    var controller = new AbortController();
    var timeout = setTimeout(function () { controller.abort(); }, 30000);
    try {
      var response = await fetch(form.action, {
        method: 'POST', credentials: 'same-origin', redirect: 'error', signal: controller.signal,
        headers: { 'Accept': 'application/json', 'Content-Type': 'application/x-www-form-urlencoded' },
        body: new URLSearchParams(new FormData(form)),
      });
      var result = await response.json();
      if (response.ok && result.kind === 'requested') {
        status.textContent = 'Refresh requested. Check again shortly for verified organization and team updates.';
      } else if (response.status === 429 && result.kind === 'throttled' && Number.isFinite(Date.parse(result.retry_at))) {
        status.textContent = 'A refresh was requested recently. Try again no earlier than ' + new Date(result.retry_at).toISOString() + '.';
      } else if (response.ok && result.kind === 'not_tracked') {
        status.textContent = 'No tracked organization invitation is available to refresh yet. This does not establish current access.';
      } else { throw new Error('Unavailable'); }
    } catch (_) {
      status.textContent = 'Organization refresh is unavailable. Check again later.';
    } finally { clearTimeout(timeout); button.disabled = false; }
  });
})();
