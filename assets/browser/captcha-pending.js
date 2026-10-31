() => {
    // Only observe completion. The site and the visitor own the challenge.
    const visible = e => e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden';
    const providers = [
        ['[name="cf-turnstile-response"]', '.cf-turnstile, iframe[src*="challenges.cloudflare.com"]', false],
        ['[name="g-recaptcha-response"]', '.g-recaptcha, iframe[src*="/recaptcha/"]', true],
        ['[name="h-captcha-response"]', '.h-captcha, iframe[src*="hcaptcha.com"]', true],
    ];
    return providers.some(([response, widgets, ignoreInvisible]) => {
        // Turnstile can render in a closed shadow root: its response field is
        // still in the form even when no widget is discoverable with selectors.
        const fields = [...document.querySelectorAll(response)].filter(field => {
            const form = field.form || field.closest('form');
            return !field.disabled && (!form || visible(form)) &&
                !(ignoreInvisible && field.closest('.grecaptcha-badge, [data-size="invisible"]'));
        });
        if (fields.length) return fields.some(field => !field.value.trim());
        // Also wait while a visible widget is loading, before its response
        // field exists. Ignore invisible badges and hidden login/ad widgets.
        return [...document.querySelectorAll(widgets)].some(widget =>
            visible(widget) && !widget.closest('.grecaptcha-badge') &&
            widget.getAttribute('data-size') !== 'invisible' &&
            !/[?&]size=invisible(?:&|$)/.test(widget.getAttribute('src') || '') &&
            widget.offsetWidth > 60 && widget.offsetHeight > 40);
    });
}
