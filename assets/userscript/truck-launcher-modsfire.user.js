// ==UserScript==
// @name         Truck Launcher — загрузка модов
// @namespace    https://github.com/frd21313123123/ets-launcher
// @version      3.18.0
// @description  Нажимает обычные бесплатные кнопки загрузки (ModsFire, Sharemods, mods.to, WDfiles, ProMods, Google Drive, TruckyMods, страницы авторов) во вкладках, которые открыл Truck Launcher. Проверки сайтов (Cloudflare, выбор цвета на WDfiles) не трогает — их проходите вы.
// @match        https://*/*
// @grant        window.close
// @grant        GM_openInTab
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

    // The script's state is shown in the tab title, e.g. "[TL 3.0: жду CAPTCHA /
    // Cloudflare]", so the visitor (and the launcher's tests) can see where it is.
    const VERSION = '3.18';
    let shown = null;
    const untitled = () => document.title.replace(/^\[TL [^\]]*\]\s*/, '');
    const status = message => {
        if (shown === message) return;
        shown = message;
        document.title = `[TL ${VERSION}: ${message}] ${untitled()}`;
    };

    // Sites such as mods.to render their page inside shadow DOM, where
    // document.querySelectorAll does not look. Every search here walks the
    // document, its shadow roots (closed ones too where the browser lets an
    // extension see them) and same-site frames.
    const roots = () => {
        const found = [];
        const visit = root => {
            found.push(root);
            for (const element of root.querySelectorAll('*')) {
                const shadow = element.openOrClosedShadowRoot || element.shadowRoot;
                if (shadow) visit(shadow);
                if (element.tagName === 'IFRAME') {
                    try {
                        if (element.contentDocument) visit(element.contentDocument);
                    } catch {
                        // A frame from another site stays closed.
                    }
                }
            }
        };
        visit(document);
        return found;
    };
    const all = selector => roots().flatMap(root => [...root.querySelectorAll(selector)]);
    const first = selector => all(selector)[0] || null;

    const host = location.hostname.replace(/^www\./, '');
    // What the visitor sees: the value of a field, otherwise the rendered text.
    const label = e => (e.tagName === 'INPUT' ? e.value : e.innerText || e.textContent || '').trim().replace(/\s+/g, ' ');
    const visible = e => e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden';
    const https = href => {
        try {
            const url = new URL(href, location.href);
            return url.protocol === 'https:' && !/(^|\.)boosty\.to$/.test(url.hostname) ? url : null;
        } catch {
            return null;
        }
    };
    // The innermost visible enabled element whose label matches, so a click
    // reaches whatever handler the site put on it.
    // Only things a visitor can press: buttons, links, fields, ARIA buttons
    // and anything showing the hand cursor - never a heading with the same words.
    const pressable = e => /^(BUTTON|A|INPUT)$/.test(e.tagName) || e.getAttribute('role') === 'button' ||
        e.hasAttribute('onclick') || getComputedStyle(e).cursor === 'pointer';
    const control = (pattern, selector = '*') => {
        const labelled = all(selector).filter(e => visible(e) && !e.disabled && pressable(e) && pattern.test(label(e)));
        return labelled.find(e => !labelled.some(other => other !== e && e.contains(other))) || null;
    };
    const fileHosts = /(^|\.)(sharemods\.com|modsfire\.com|mods\.to|wdfiles\.ru|drive\.google\.com|drive\.usercontent\.google\.com|truckymods\.io)$/;

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
    // Same completion check as assets/browser/captcha-pending.js, also
    // searching shadow roots. Self-contained, so tests/browser_captcha.cjs can
    // run it on its own. It only observes: the site and the visitor own the
    // challenge.
    const checkPending = () => {
        const visible = e => e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden';
        const all = selector => {
            const found = [];
            const visit = root => {
                found.push(...root.querySelectorAll(selector));
                for (const element of root.querySelectorAll('*')) {
                    const shadow = element.openOrClosedShadowRoot || element.shadowRoot;
                    if (shadow) visit(shadow);
                }
            };
            visit(document);
            return found;
        };
        const providers = [
            ['[name="cf-turnstile-response"]', '.cf-turnstile, iframe[src*="challenges.cloudflare.com"]', false],
            ['[name="g-recaptcha-response"]', '.g-recaptcha, iframe[src*="/recaptcha/"]', true],
            ['[name="h-captcha-response"]', '.h-captcha, iframe[src*="hcaptcha.com"]', true],
        ];
        return providers.some(([response, widgets, ignoreInvisible]) => {
            const fields = all(response).filter(field => {
                const form = field.form || field.closest('form');
                return !field.disabled && (!form || visible(form)) &&
                    !(ignoreInvisible && field.closest('.grecaptcha-badge, [data-size="invisible"]'));
            });
            if (fields.length) return fields.some(field => !field.value.trim());
            return all(widgets).some(widget =>
                visible(widget) && !widget.closest('.grecaptcha-badge') &&
                widget.getAttribute('data-size') !== 'invisible' &&
                !/[?&]size=invisible(?:&|$)/.test(widget.getAttribute('src') || '') &&
                widget.offsetWidth > 60 && widget.offsetHeight > 40);
        });
    };
    const finished = () => {
        done = true;
        clearInterval(timer);
        status('загрузка запущена, вкладка закроется');
        sessionStorage.removeItem(MARK);
        // The download continues in the browser's download manager, and the
        // launcher opens the next file as soon as it sees the download start.
        // File servers can take 10 s or more to answer, and closing the tab
        // before then cancels the request, so the tab stays a while.
        setTimeout(() => window.close(), 30000);
    };
    // A page whose layout the script does not know: say what is there instead
    // of failing silently.
    const unknown = () => {
        // Elements that look like a download step, with what keeps them from
        // being pressed: tag, disabled, hidden.
        const candidates = all('*')
            .filter(e => e.children.length <= 2 && /\b(begin|download|start|continue|get link)\b/i.test(label(e)) && label(e).length < 40)
            .slice(0, 3)
            .map(e => `${e.tagName.toLowerCase()} "${label(e)}"${e.disabled ? ' disabled' : ''}${visible(e) ? '' : ' hidden'}`);
        status(`не нашёл кнопку загрузки${candidates.length ? ' (' + candidates.join('; ') + ')' : ''}`);
        // Once per page, if it stays unknown for half a minute (pages often
        // pass through a moment with no buttons): save the page's controls to
        // the Downloads folder so a new layout can be supported. Structure and
        // labels only - no field values, tokens or cookies.
        if (readyAt === null || Date.now() - readyAt < 30000) return;
        once('report:' + location.pathname, () => {
            const describe = e => {
                const style = getComputedStyle(e);
                return [
                    e.tagName.toLowerCase() + (e.id ? '#' + e.id : '') + (typeof e.className === 'string' && e.className ? '.' + e.className.trim().split(/\s+/).slice(0, 4).join('.') : ''),
                    e.getAttribute('role') ? 'role=' + e.getAttribute('role') : '',
                    e.getAttribute('type') ? 'type=' + e.getAttribute('type') : '',
                    e.disabled ? 'disabled' : '',
                    visible(e) ? '' : 'hidden',
                    'cursor=' + style.cursor,
                    e.shadowRoot || e.openOrClosedShadowRoot ? 'shadow' : '',
                    e.tagName === 'A' ? 'href=' + (https(e.href)?.pathname || '') : '',
                    e.tagName === 'IFRAME' ? 'frame=' + (https(e.src)?.hostname || '') : '',
                    JSON.stringify(e.tagName === 'INPUT' ? '' : label(e).slice(0, 60)),
                ].filter(Boolean).join(' ');
            };
            const lines = all('*')
                .filter(e => e.tagName === 'IFRAME' || e.shadowRoot || e.openOrClosedShadowRoot ||
                    ((e.children.length <= 3) && (pressable(e) || /\b(begin|download|start|continue|link)\b/i.test(label(e))) && label(e).length < 80))
                .slice(0, 300)
                .map(describe);
            const report = [`Truck Launcher ${VERSION}`, `${location.hostname}${location.pathname}`, `roots: ${roots().length}`, '', ...lines].join('\n');
            const link = document.createElement('a');
            link.href = URL.createObjectURL(new Blob([report], { type: 'text/plain' }));
            link.download = `truck-launcher-debug-${location.hostname}-${Date.now()}.txt`;
            document.body.appendChild(link);
            link.click();
            link.remove();
        });
    };

    const timer = setInterval(() => {
        if (done || document.readyState === 'loading') return;
        if (readyAt === null) readyAt = Date.now();
        // Cookie walls cover the page; they get the most private answer.
        if (!consent) {
            const reject = control(/^(reject all|decline all|refuse all|отклонить все|отклонить)$/i, 'button, a[role="button"], [role="button"]');
            if (reject) { consent = true; reject.click(); }
        }
        if (checkPending()) {
            status('жду CAPTCHA / Cloudflare');
            const widget = first('.cf-turnstile, .g-recaptcha, .h-captcha, iframe[src*="challenges.cloudflare.com"], iframe[src*="/recaptcha/"], iframe[src*="hcaptcha.com"]');
            if (widget) once('show-check', () => widget.scrollIntoView({ block: 'center' }));
            return;
        }
        // A host's own human check works the same way: WDfiles asks to press the
        // button of a named colour. The visitor answers it; the script waits.
        const colours = all('button.btn-color').filter(visible);
        if (colours.length > 1) {
            status('жду проверку сайта (выберите цвет)');
            once('show-colours', () => colours[0].scrollIntoView({ block: 'center' }));
            readyAt = Date.now();
            return;
        }

        if (host === 'promods.net') {
            // setup.php: the free "Обычная загрузка" (never the paid fast one),
            // then the download step itself.
            // The choice buttons are styled blocks (div.pm-choice-btn.free).
            const free = all('.pm-choice-btn.free').find(visible) ||
                control(/^(обычная загрузка|regular download|normal download|free download)/i, 'a[href], button, .pm-choice-btn');
            if (free) {
                if (once('promods-free', () => free.click())) status(`нажал «${label(free)}»`);
                return;
            }
            // "Скачать файлы": one "СКАЧАТЬ" link per part of the split archive,
            // with the site's 5 minutes between parts. Each part opens in a
            // background tab carrying the mark (a script's new tab would be a
            // blocked popup), and this page stays to open the next one.
            const parts = all('a.pm-dl-btn.free[href]').filter(visible);
            if (parts.length) {
                const key = 'tl-promods-opened';
                const opened = JSON.parse(sessionStorage.getItem(key) || '[]');
                const next = parts.find(a => !opened.includes(a.href));
                if (!next) { finished(); return; }
                const part = `часть ${opened.length + 1} из ${parts.length}`;
                const last = Number(sessionStorage.getItem(`${key}:at`) || 0);
                const waiting = next.classList.contains('disabled') || next.getAttribute('aria-disabled') === 'true' ||
                    getComputedStyle(next).pointerEvents === 'none' || (opened.length > 0 && Date.now() - last < 5 * 60 * 1000 + 5000);
                if (waiting) {
                    const left = Math.max(0, Math.ceil((last + 5 * 60 * 1000 + 5000 - Date.now()) / 60000));
                    status(`ProMods: жду паузу сайта (${part}${left ? `, ~${left} мин` : ''})`);
                    return;
                }
                const url = https(next.href);
                if (!url) { unknown(); return; }
                url.hash = MARK;
                opened.push(next.href);
                sessionStorage.setItem(key, JSON.stringify(opened));
                sessionStorage.setItem(`${key}:at`, String(Date.now()));
                GM_openInTab(url.href, { active: false, insert: true });
                status(`ProMods: открыл ${part}`);
                return;
            }
            // getdl.php: "Download promods-….7z.001" starts the part by itself.
            if (location.pathname === '/getdl.php') {
                if (Date.now() - readyAt > 60000) finished(); else status('ProMods: часть скачивается');
                return;
            }
            unknown();
            return;
        }

        // An author's own page: follow its first "Download" link. A link to a
        // file host gets the mark carried along; a link straight to a mod file
        // (.scs, .zip, .rar, .7z) is simply downloaded.
        if (!fileHosts.test(host)) {
            const fileLink = url => /\.(scs|zip|rar|7z|\d{3})$/i.test(url.pathname);
            const link = all('a[href]').find(a => {
                const url = https(a.href);
                const labelled = /^\s*(download|скачать|pobierz)(?![\p{L}\p{N}])/iu.test(label(a)) || /download/i.test(a.className);
                return url && labelled && (fileHosts.test(url.hostname) || fileLink(url));
            });
            if (!link) { status('ищу ссылку на файл'); return; }
            const url = https(link.href);
            if (fileLink(url) && !fileHosts.test(url.hostname)) {
                link.removeAttribute('target');
                link.click();
                finished();
            } else {
                done = true;
                status('перехожу к файлу');
                url.hash = MARK;
                location.assign(url.href);
            }
            return;
        }

        if (host === 'truckymods.io' || host === 'beta.truckymods.io') {
            // The mod's own "Download (5.7 MB)" link to …/download/latest (or a
            // build number); banners for Trucky's apps and unlabeled icon links
            // to older builds are left alone. Some mods need a signed-in
            // account, which the visitor's browser has.
            const page = location.pathname.replace(/\/$/, '');
            const own = all('a[href]').filter(a => {
                const url = https(a.href);
                return url && url.hostname === location.hostname && url.pathname.startsWith(page + '/download/');
            });
            const link = own.find(a => https(a.href).pathname.endsWith('/download/latest'))
                || own.find(a => /\/download\/\d+$/.test(https(a.href).pathname) && /^download\b/i.test(label(a)));
            if (link) { link.click(); finished(); } else { unknown(); }
            return;
        }

        if (host === 'modsfire.com') {
            // "Generate Link" (its address removed by the site), then the same
            // button becomes "Download Link"; the next page has "Download".
            const ready = all('a.download-button[href]').find(a => /^download link$/i.test(label(a)));
            if (ready && https(ready.getAttribute('href'))) {
                done = true;
                status('перехожу к файлу');
                location.assign(https(ready.getAttribute('href')).href);
                return;
            }
            const generate = control(/^generate\s+(download\s+)?link$/i);
            if (generate) {
                if (once('generate', () => generate.click())) status('создаю ссылку');
                return;
            }
            const download = first('a#downloadnow[href]');
            if (download) { download.click(); finished(); return; }
            if (!actions.has('generate')) unknown();
            return;
        }

        if (host === 'drive.usercontent.google.com' || host === 'drive.google.com') {
            // The "download anyway" confirmation for large files.
            const confirm = first('#uc-download-link, form#download-form button, form#download-form input[type="submit"]');
            if (confirm) { confirm.click(); finished(); } else { unknown(); }
            return;
        }

        if (host === 'wdfiles.ru') {
            // After the colour check: "Скачать / Смотреть", which leads back to
            // the same address with the download allowed. Pressed at most three
            // times per file, so a page that keeps coming back cannot loop forever.
            const file = all('a[href]').find(a => visible(a) && /\.(scs|zip|rar|7z|\d{3})(\?|$)/i.test(https(a.href)?.pathname || ''));
            if (file) {
                location.assign(file.href);
                finished();
                return;
            }
            // Each load runs a short timer first; the first press reloads the
            // page, the second asks "download the file?" and then downloads.
            // The button is a plain <a onclick> with no address of its own.
            if (Date.now() - readyAt < 7000) { status('жду таймер сайта'); return; }
            const button = control(/^скачать(?![\p{L}])/iu, 'a[href], a[onclick], button, input[type="submit"]');
            const key = `tl-wdfiles:${location.pathname}`;
            const pressed = Number(sessionStorage.getItem(key) || 0);
            if (button && pressed < 3) {
                // The site's own "download the file?" question gets "yes";
                // any other question still goes to the visitor. A page
                // script, as the question is asked by the page itself.
                once('wdfiles-confirm', () => {
                    const script = document.createElement('script');
                    script.textContent = `(() => { const ask = window.confirm.bind(window);
                        window.confirm = message => /скача|загруз|download/i.test(String(message)) || ask(message); })();`;
                    document.documentElement.appendChild(script);
                    script.remove();
                });
                once('wdfiles-download', () => {
                    sessionStorage.setItem(key, String(pressed + 1));
                    button.click();
                });
                // After the second press the download starts (or the page
                // loads again and this script carries on there).
                if (pressed > 0) finished(); else status(`нажал «${label(button)}»`);
                return;
            }
            // Pressed twice already: the download was asked for.
            if (pressed >= 2) finished(); else unknown();
            return;
        }

        // Sharemods, mods.to and other XFileSharing hosts: a direct link first.
        // mods.to's last step is "Download link generated" with a#download-link;
        // Sharemods' is a "DOWNLOAD [48 KB]" link carrying the file size.
        const direct = first('a#downloadnow[href], a#download_link[href], a#download-link[href]') ||
            all('a[href]').find(a => visible(a) && /^(start download(?:\s|$)|download\s*\[\s*[\d.,]+\s*[kmgt]?b\s*\])/i.test(label(a)));
        if (direct && https(direct.href) && https(direct.href).href !== location.href) {
            // Go to the file in this tab instead of clicking: a script's click
            // on a new-tab link is a blocked popup when the page is in the
            // background, and Sharemods' link does nothing when clicked without
            // its new tab. The file address itself starts the download.
            location.assign(direct.href);
            finished();
            return;
        }
        // Then the free button, after the host's own timer.
        if (Date.now() - readyAt < 15000) { status('жду таймер сайта'); return; }
        const countdown = first('#countdown, #countdown_str, .countdown');
        if (countdown && visible(countdown) && /\d+/.test(countdown.textContent) && Number(countdown.textContent.match(/\d+/)[0]) > 0) {
            status('жду таймер сайта');
            return;
        }
        if (host === 'mods.to') {
            // Step pages: "Start now" (after Cloudflare's check), then
            // "Download file [1]" with "Begin Downloand" (sic), then the next step.
            // In order of the site's steps; a label is compared by its start, as
            // an icon or hidden helper text may follow the words, and the site's
            // own spelling varies ("Downloand"), so only the stem is matched.
            const step = [/^start now/i, /^begin\s*down/i, /^continue( down\w*)?(?![\p{L}])/iu, /^down\w* now/i, /^get (down\w* )?link/i, /^down\w*(?![\p{L}])/iu]
                .map(pattern => control(pattern))
                .find(Boolean) ||
                // Each step is a single form with one submit button and no fields
                // to fill in; press it whatever the site calls it this time.
                (() => {
                    const forms = all('form').filter(f => visible(f) && !f.querySelector('input[type="password"], input[type="email"], input[type="text"], textarea'));
                    const buttons = forms.flatMap(f => [...f.querySelectorAll('button[type="submit"], button:not([type]), input[type="submit"]')]).filter(b => visible(b) && !b.disabled);
                    return buttons.length === 1 ? buttons[0] : null;
                })();
            if (step) {
                if (once(location.pathname + ':' + label(step).toLowerCase(), () => step.click())) status(`нажал «${label(step)}»`);
            } else {
                unknown();
            }
            return;
        }
        const form = roots().flatMap(root => [...root.querySelectorAll('form')])
            .find(f => ['download1', 'download2'].includes(f.querySelector('input[name="op"]')?.value));
        if (!form || form.querySelector('input[type="password"]')) { unknown(); return; }
        // `method_free` is often a hidden field; only real submit controls count.
        const button = form.querySelector('button[name="method_free"], input[type="submit"][name="method_free"]') || [...form.querySelectorAll('button, input[type="submit"]')].find(b => !/premium|paid/i.test(label(b)) && /^(create download link|free download|start download|download)(?:\s|$)/i.test(label(b)));
        const op = form.querySelector('input[name="op"]').value;
        if (button && !button.disabled) {
            if (once(op, () => button.click())) status(`нажал «${label(button)}»`);
        } else {
            unknown();
        }
    }, 1000);
    // CAPTCHA and Cloudflare's check wait for the visitor, who may come back much later.
    setTimeout(() => clearInterval(timer), 3 * 60 * 60 * 1000);
})();
