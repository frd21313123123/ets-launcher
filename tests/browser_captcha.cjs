// No browser automation or live CAPTCHA solving: exercise the response lifecycle
// with the same page script that is embedded in WebView2 and in Tampermonkey.
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const { test } = require('node:test');

const root = join(__dirname, '..');
const embedded = readFileSync(join(root, 'assets/browser/captcha-pending.js'), 'utf8');
const userscript = readFileSync(join(root, 'assets/userscript/truck-launcher-modsfire.user.js'), 'utf8');
const installed = userscript.match(/const checkPending = ([\s\S]+?);\s*const finished =/)[1];

function element(options = {}) {
    return {
        value: '', disabled: false, offsetWidth: 300, offsetHeight: 80,
        getClientRects: () => options.hidden ? [] : [{}],
        closest: selector => options.ancestors?.[selector] || null,
        getAttribute: name => options.attributes?.[name] || null,
        ...options,
    };
}

function page(provider, { field = true, widget = true, ...options } = {}) {
    const input = element(options);
    const view = element(options.widgetOptions);
    const response = `[name="${provider}-response"]`;
    const widgets = { 'cf-turnstile': '.cf-turnstile', 'g-recaptcha': '.g-recaptcha', 'h-captcha': '.h-captcha' }[provider];
    const document = {
        querySelectorAll: selector => {
            if (selector === response) return field ? [input] : [];
            if (selector.startsWith(widgets + ',')) return widget ? [view] : [];
            return [];
        },
    };
    return { input, view, document, getComputedStyle: () => ({ visibility: 'visible' }) };
}

for (const [name, source] of [['WebView2', embedded], ['Tampermonkey', installed]]) {
    const check = state => vm.runInNewContext(`(${source})()`, state);
    for (const provider of ['cf-turnstile', 'g-recaptcha', 'h-captcha']) {
        test(`${name}: ${provider} waits, resumes with the widget still present, and waits after expiry`, () => {
            const state = page(provider);
            assert.equal(check(state), true);
            state.input.value = 'site-issued-test-response';
            assert.equal(check(state), false);
            state.input.value = '';
            assert.equal(check(state), true);
        });
        test(`${name}: ${provider} response field works without a discoverable widget (Shadow DOM)`, () => {
            const state = page(provider, { widget: false });
            assert.equal(check(state), true);
            state.input.value = 'site-issued-test-response';
            assert.equal(check(state), false);
        });
        test(`${name}: ${provider} waits while a visible widget creates its response field`, () => {
            assert.equal(check(page(provider, { field: false })), true);
        });
        test(`${name}: hidden ${provider} login widgets do not block a download`, () => {
            const state = page(provider, { form: element({ hidden: true }), widgetOptions: { hidden: true } });
            assert.equal(check(state), false);
        });
    }
    test(`${name}: invisible reCAPTCHA does not deadlock the button that invokes it`, () => {
        const state = page('g-recaptcha', {
            ancestors: { '.grecaptcha-badge, [data-size="invisible"]': element() },
            widgetOptions: { attributes: { 'data-size': 'invisible' } },
        });
        assert.equal(check(state), false);
    });
    test(`${name}: no CAPTCHA does not block a download`, () => {
        assert.equal(check(page('cf-turnstile', { field: false, widget: false })), false);
    });
    test(`${name}: a solved widget cannot hide another unfinished response`, () => {
        const state = page('cf-turnstile');
        state.document.querySelectorAll = selector => selector === '[name="cf-turnstile-response"]'
            ? [element({ value: 'site-issued-test-response' }), state.input] : [];
        assert.equal(check(state), true);
    });
}

test('WebView2: the download button is clicked once, only after the visitor finishes the check', () => {
    const rust = readFileSync(join(root, 'src/mod_browser.rs'), 'utf8');
    const automation = rust.match(/const QUPER_FREE_DOWNLOAD: &str = r#"([\s\S]+?)"#;/)[1];
    const state = page('cf-turnstile', { widget: false });
    let tick, now = 0, clicks = 0;
    const selectAll = state.document.querySelectorAll;
    const button = element({ textContent: 'Start now', click: () => { clicks++; } });
    Object.assign(state.document, {
        readyState: 'complete',
        querySelector: () => null,
        querySelectorAll: selector => selector === 'button, a[href]' ? [button] : selectAll(selector),
    });
    Object.assign(state, {
        location: new URL('https://mods.to/file/test'),
        window: {},
        Date: { now: () => now },
        setInterval: callback => { tick = callback; return 1; },
        clearInterval: () => {}, setTimeout: () => {},
    });
    vm.runInNewContext(`const captchaPending = ${embedded}; ${automation}`, state);
    tick();
    now = 16000;
    tick();
    assert.equal(clicks, 0, 'must not submit a form with an empty Turnstile response');
    state.input.value = 'site-issued-test-response';
    tick();
    assert.equal(clicks, 1, 'must continue after completion');
    tick();
    assert.equal(clicks, 1, 'must not submit twice');
});
