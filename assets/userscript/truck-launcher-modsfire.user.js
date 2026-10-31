// ==UserScript==
// @name         Truck Launcher — ModsFire
// @namespace    https://github.com/frd21313123123/ets-launcher
// @version      1.0.0
// @description  Нажимает обычные бесплатные кнопки ModsFire во вкладках, которые открыл Truck Launcher. Проверку Cloudflare не трогает — если она попросит, отметьте её сами.
// @match        https://modsfire.com/*
// @match        https://www.modsfire.com/*
// @grant        window.close
// @run-at       document-idle
// ==/UserScript==

(() => {
    'use strict';
    // Only tabs the launcher opened: it adds this mark to the address. The mark
    // is kept for the tab, so the following steps of the same file are handled.
    const MARK = 'truck-launcher';
    if (location.hash.includes(MARK)) sessionStorage.setItem(MARK, '1');
    if (sessionStorage.getItem(MARK) !== '1') return;

    const text = e => (e.textContent || '').trim().replace(/\s+/g, ' ');
    const sameSite = href => {
        try {
            const url = new URL(href, location.href);
            return url.protocol === 'https:' && url.origin === location.origin ? url : null;
        } catch {
            return null;
        }
    };
    let generated = false;
    let started = false;
    const timer = setInterval(() => {
        if (document.readyState === 'loading' || started) return;
        // Cloudflare's own check page has none of these buttons, so nothing
        // happens until the visitor passes it and the real page loads.

        // Step 1: "Generate Link" (its address is removed by the site), then the
        // same button becomes "Download Link" after the site's short timer.
        const ready = [...document.querySelectorAll('a.download-button[href]')].find(a => /^download link$/i.test(text(a)));
        if (ready) {
            const url = sameSite(ready.getAttribute('href'));
            if (url) { started = true; location.assign(url.href); }
            return;
        }
        if (!generated) {
            const generate = [...document.querySelectorAll('a, button')].find(b => /^generate\s+(download\s+)?link$/i.test(text(b)));
            if (generate && !generate.disabled) { generated = true; generate.click(); return; }
        }

        // Step 2: the free "Download" button starts the file download.
        const download = document.querySelector('a#downloadnow[href]');
        if (download && sameSite(download.getAttribute('href'))) {
            started = true;
            clearInterval(timer);
            download.click();
            // The download continues in the browser; the tab is no longer needed.
            sessionStorage.removeItem(MARK);
            setTimeout(() => window.close(), 15000);
        }
    }, 1000);
    setTimeout(() => clearInterval(timer), 15 * 60 * 1000);
})();
