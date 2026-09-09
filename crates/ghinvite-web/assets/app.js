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

/* Invitation-code shortcut (home page, signed in): open /i/{code} from the
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
    var code = input.value.trim();
    if (!code) {
      if (message) message.textContent = 'Enter an invitation code first.';
      input.focus();
      return;
    }
    if (message) message.textContent = '';
    window.location.href = '/i/' + encodeURIComponent(code);
  }
})();
