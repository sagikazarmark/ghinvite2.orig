import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const script = readFileSync(new URL('../../assets/i.js', import.meta.url), 'utf8');
const marker = Buffer.from('SECRET_MARKER_01234567890123456789').subarray(0, 32).toString('base64url');

function browser(fragment, { ready = 'loading', csrf = 'csrf-fixture', formPresent = true, historyError = false } = {}) {
  const events = [];
  const requests = [];
  const callbacks = [];
  const input = { value: '' };
  class Form {
    method = 'get';
    action = 'https://wrong.example/leak';
    target = '_blank';
    querySelector(selector) {
      return selector === 'input[name="csrf_token"]' ? { value: csrf } : input;
    }
    submit() {
      events.push('submit');
      requests.push({ url: this.action, method: this.method, target: this.target,
        encoding: this.enctype, body: new URLSearchParams({ secret: input.value, csrf_token: csrf }).toString() });
    }
  }
  const form = new Form();
  const window = {
    location: { hash: fragment },
    history: { replaceState(state, unused, url) {
      events.push('replace');
      assert.equal(state, null);
      assert.equal(url, '/i');
      if (historyError) throw new Error('history denied');
      window.location.hash = '';
    } },
  };
  const document = {
    readyState: ready,
    getElementById(id) { assert.equal(id, 'invitation-secret-form'); return formPresent ? form : null; },
    addEventListener(event, callback, options) {
      events.push('listen');
      assert.equal(event, 'DOMContentLoaded');
      assert.equal(options.once, true);
      callbacks.push(callback);
    },
  };
  const context = vm.createContext({ window, document, HTMLFormElement: Form,
    fetch() { assert.fail('fragment reader must use native POST'); },
    console: { log() { assert.fail('secret must never enter diagnostics'); }, error() { assert.fail('secret must never enter diagnostics'); } },
  });
  vm.runInContext(script, context);
  return { events, requests, input, window, ready() { callbacks.forEach(callback => callback()); } };
}

test('clears fragment synchronously, posts only body with CSRF, and removes DOM plaintext', () => {
  const b = browser(`#${marker}`);
  assert.deepEqual(b.events, ['replace', 'listen']);
  assert.equal(b.window.location.hash, '');
  assert.equal(b.input.value, '');
  assert.deepEqual(b.requests, []);
  b.ready();
  assert.deepEqual(b.events, ['replace', 'listen', 'submit']);
  assert.equal(b.requests.length, 1);
  const request = b.requests[0];
  assert.equal(request.url, '/i');
  assert.equal(request.method, 'post');
  assert.equal(request.target, '_self');
  assert.equal(request.encoding, 'application/x-www-form-urlencoded');
  assert.deepEqual([...new URLSearchParams(request.body)], [['secret', marker], ['csrf_token', 'csrf-fixture']]);
  assert.equal(b.input.value, '');
  assert.ok(!JSON.stringify(b.events).includes(marker));
});

test('already parsed document still clears history before submitting', () => {
  const b = browser(`#${marker}`, { ready: 'complete' });
  assert.deepEqual(b.events, ['replace', 'submit']);
});

test('invalid fragments are erased without a request or echoed error', () => {
  for (const secret of ['', '01ARZ3NDEKTSV4RRFFQ69G5FAV', `${marker}=`, ` ${marker}`, `${marker}\n`, marker.replace(/.$/, 'B'), marker.replace(/.$/, '/'), `%41${marker.slice(1)}`, `${marker}&csrf_token=evil`]) {
    const b = browser(`#${secret}`);
    b.ready();
    assert.deepEqual(b.events, ['replace']);
    assert.deepEqual(b.requests, []);
    assert.equal(b.window.location.hash, '');
    assert.equal(b.input.value, '');
  }
});

test('missing CSRF/form or history failure never transmits the secret', () => {
  for (const options of [{ csrf: '' }, { formPresent: false }, { historyError: true }]) {
    const b = browser(`#${marker}`, options);
    b.ready();
    assert.deepEqual(b.requests, []);
    assert.equal(b.input.value, '');
  }
});

test('home shortcut uses only a canonical fragment, rejects legacy ULID', () => {
  const source = readFileSync(new URL('../../assets/app.js', import.meta.url), 'utf8');
  const start = source.indexOf('/* Invitation-code shortcut');
  assert.ok(start >= 0);
  for (const secret of [marker, '01ARZ3NDEKTSV4RRFFQ69G5FAV', `${marker}=`, ` ${marker}`]) {
    const handlers = {};
    const input = { value: secret, focus() {} };
    const message = { textContent: '' };
    const window = { location: { href: '/' } };
    const document = {
      addEventListener(event, handler) { handlers[event] = handler; },
      querySelector(selector) { return selector === '[data-invitation-code-input]' ? input : message; },
    };
    vm.runInNewContext(source.slice(start), { window, document });
    handlers.click({ target: { closest() { return {}; } } });
    if (secret === marker) {
      assert.equal(window.location.href, `/i#${marker}`);
      assert.equal(input.value, '');
      assert.equal(new URL(window.location.href, 'https://p1.example').pathname, '/i');
    } else {
      assert.equal(window.location.href, '/');
      assert.equal(message.textContent, 'Enter a valid invitation code.');
    }
  }
});
