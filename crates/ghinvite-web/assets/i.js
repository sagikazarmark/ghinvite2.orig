/* Loaded synchronously on /i from this origin. No secret is put in a request
 * URL, browser storage, diagnostics, or an asynchronous callback's argument. */
(function () {
  'use strict';
  var secret = window.location.hash.slice(1);
  try {
    // Clear even malformed fragments before validation, DOM readiness or I/O.
    window.history.replaceState(null, '', '/i');
  } catch (_) {
    secret = '';
    return;
  }
  if (!/^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$/.test(secret)) {
    secret = '';
    return;
  }

  function post() {
    var form = document.getElementById('invitation-secret-form');
    if (!(form instanceof HTMLFormElement)) {
      secret = '';
      return;
    }
    var csrf = form.querySelector('input[name="csrf_token"]');
    var input = form.querySelector('input[name="secret"]');
    if (!csrf || !csrf.value || !input) {
      secret = '';
      return;
    }
    // A native navigation follows POST /i's 303 to /join. Keep the form's
    // request target fixed and avoid fetching/rendering any response body.
    form.method = 'post';
    form.action = '/i';
    form.target = '_self';
    form.enctype = 'application/x-www-form-urlencoded';
    input.value = secret;
    secret = '';
    try {
      HTMLFormElement.prototype.submit.call(form);
    } finally {
      input.value = '';
    }
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', post, { once: true });
  } else {
    post();
  }
})();
