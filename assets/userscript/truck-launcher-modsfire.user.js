// ==UserScript==
// @name         Truck Launcher — загрузка модов
// @namespace    https://github.com/frd21313123123/ets-launcher
// @version      2.1.0
// @description  Нажимает обычные бесплатные кнопки загрузки (ModsFire, Sharemods, mods.to, Google Drive, TruckyMods, страницы авторов) во вкладках, которые открыл Truck Launcher. Проверку Cloudflare не трогает — если она попросит, отметьте её сами.
// @match        https://*/*
// @grant        window.close
// @run-at       document-idle
// ==/UserScript==

(() => {
    'use strict';
    // Only tabs the launcher opened: it adds this mark to the address. The mark
    // is kept for the tab and passed on when an author's page leads to a file host.
    const MARK = 'truck-launcher';
    if (window.top !== window) return;
    if (location.hash.includes(MARK)) sessionStorage.setItem(MARK, '1');
    if (sessionStorage.getItem(MARK) !== '1') return;

    const host = location.hostname.replace(/^www\./, '');
    const text = e => (e.textContent || e.value || '').trim().replace(/\s+/g, ' ');
    const visible = e => e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden';
    const https = href => {
        try {
            const url = new URL(href, location.href);
            return url.protocol === 'https:' && !/(^|\.)boosty\.to$/.test(url.hostname) ? url : null;
        } catch {
            return null;
        }
    };
    const fileHosts = /(^|\.)(sharemods\.com|modsfire\.com|mods\.to|wdfiles\.ru|drive\.google\.com|drive\.usercontent\.google\.com|truckymods\.io)$/;
    // A Turnstile widget hands the page a token once the visitor is verified;
    // forms are only sent after that. The widget itself is never touched.
    const checkPending = () =>
        document.querySelector('.cf-turnstile, iframe[src*="challenges.cloudflare.com"]') &&
        ![...document.querySelectorAll('input[name="cf-turnstile-response"]')].some(t => t.value.length > 20);
    const finished = () => {
        sessionStorage.removeItem(MARK);
        // The download continues in the browser; the tab is no longer needed.
        setTimeout(() => window.close(), 15000);
    };

    let done = false;
    let readyAt = null;
    let consent = false;
    const actions = new Set();
    const once = (key, action) => {
        if (actions.has(key)) return false;
        actions.add(key);
        action();
        return true;
    };
    const timer = setInterval(() => {
        if (done || document.readyState === 'loading') return;
        if (readyAt === null) readyAt = Date.now();
        // Cookie walls cover the page; they get the most private answer.
        if (!consent) {
            const reject = [...document.querySelectorAll('button, a[role="button"]')].find(b => visible(b) && /^(reject all|decline all|refuse all|отклонить все|отклонить)$/i.test(text(b)));
            if (reject) { consent = true; reject.click(); }
        }
        if (checkPending()) {
            const widget = document.querySelector('.cf-turnstile, iframe[src*="challenges.cloudflare.com"]');
            once('show-check', () => widget.scrollIntoView({ block: 'center' }));
            return;
        }

        // An author's own page: follow its first "Download" link. A link to a
        // file host gets the mark carried along; a link straight to a mod file
        // (.scs, .zip, .rar, .7z) is simply downloaded.
        if (!fileHosts.test(host)) {
            const fileLink = url => /\.(scs|zip|rar|7z|\d{3})$/i.test(url.pathname);
            const link = [...document.querySelectorAll('a[href]')].find(a => {
                const url = https(a.href);
                const labelled = /^\s*(download|скачать|pobierz)\b/i.test(text(a)) || /download/i.test(a.className);
                return url && labelled && (fileHosts.test(url.hostname) || fileLink(url));
            });
            if (link) {
                done = true;
                const url = https(link.href);
                if (fileLink(url) && !fileHosts.test(url.hostname)) {
                    clearInterval(timer);
                    link.removeAttribute('target');
                    link.click();
                    finished();
                } else {
                    url.hash = MARK;
                    location.assign(url.href);
                }
            }
            return;
        }

        if (host === 'truckymods.io' || host === 'beta.truckymods.io') {
            // Some mods need a signed-in account; the visitor's browser has it.
            const button = [...document.querySelectorAll('downloadbutton button, downloadbutton a, button, a.btn')].find(b => visible(b) && /^download\b/i.test(text(b)) && !/premium|client|install/i.test(text(b)));
            if (button && once('trucky', () => button.click())) finished();
            return;
        }

        if (host === 'modsfire.com') {
            // "Generate Link" (its address removed by the site), then the same
            // button becomes "Download Link"; the next page has "Download".
            const ready = [...document.querySelectorAll('a.download-button[href]')].find(a => /^download link$/i.test(text(a)));
            if (ready && https(ready.getAttribute('href'))) { done = true; location.assign(https(ready.getAttribute('href')).href); return; }
            const generate = [...document.querySelectorAll('a, button')].find(b => /^generate\s+(download\s+)?link$/i.test(text(b)));
            if (generate && once('generate', () => generate.click())) return;
            const download = document.querySelector('a#downloadnow[href]');
            if (download) { done = true; clearInterval(timer); download.click(); finished(); }
            return;
        }

        if (host === 'drive.usercontent.google.com' || host === 'drive.google.com') {
            // The "download anyway" confirmation for large files.
            const confirm = document.querySelector('#uc-download-link, form#download-form button, form#download-form input[type="submit"]');
            if (confirm) { done = true; clearInterval(timer); confirm.click(); finished(); }
            return;
        }

        // Sharemods, mods.to and other XFileSharing hosts: a direct link first.
        const direct = document.querySelector('a#downloadnow[href], a#download_link[href]') || [...document.querySelectorAll('a[href]')].find(a => /^start download(?:\s|$)/i.test(text(a)));
        if (direct && https(direct.href) && https(direct.href).href !== location.href) {
            done = true; clearInterval(timer); direct.click(); finished(); return;
        }
        // Then the free button, after the host's own timer.
        if (Date.now() - readyAt < 15000) return;
        const countdown = document.querySelector('#countdown, #countdown_str, .countdown');
        if (countdown && /\d+/.test(countdown.textContent) && Number(countdown.textContent.match(/\d+/)[0]) > 0) return;
        if (host === 'mods.to') {
            const start = [...document.querySelectorAll('button, a[href]')].find(b => /^start now$/i.test(text(b)));
            if (start && !start.disabled) once('start-now', () => start.click());
            return;
        }
        const form = [...document.forms].find(f => ['download1', 'download2'].includes(f.querySelector('input[name="op"]')?.value));
        if (!form || form.querySelector('input[type="password"]')) return;
        // `method_free` is often a hidden field; only real submit controls count.
        const button = form.querySelector('button[name="method_free"], input[type="submit"][name="method_free"]') || [...form.querySelectorAll('button, input[type="submit"]')].find(b => !/premium|paid/i.test(text(b)) && /^(create download link|free download|start download|download)(?:\s|$)/i.test(text(b)));
        const op = form.querySelector('input[name="op"]').value;
        if (button && !button.disabled) once(op, () => button.click());
    }, 1000);
    setTimeout(() => clearInterval(timer), 15 * 60 * 1000);
})();
