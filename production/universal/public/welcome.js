// External file, not inline: FlashLin's default security headers send
// Content-Security-Policy: script-src 'self' (no 'unsafe-inline'), which
// silently blocks inline <script> blocks and inline onclick="..." handlers.
// A same-origin external file like this one is allowed by that policy
// without weakening it for the rest of the site — do not move this logic
// back inline without also relaxing script-src for every page it applies to.

// Measures real, this-visit page load time with the Navigation Timing API
// (not a synthetic/marketing number) and labels it the way a visitor would
// judge it: Blazing fast / Fast / Moderate / Slow. Thresholds are plain
// wall-clock time to fully load this page, including the network round
// trip — closer to "did this feel fast" than a lab-only server-side timing.
function reportLoadSpeed() {
    var ms = null;
    var nav = performance.getEntriesByType && performance.getEntriesByType('navigation')[0];
    if (nav && nav.loadEventEnd > 0) {
        ms = Math.round(nav.loadEventEnd - nav.startTime);
    } else if (performance.timing && performance.timing.loadEventEnd > 0) {
        ms = performance.timing.loadEventEnd - performance.timing.navigationStart;
    }

    var badge = document.getElementById('speedBadge');
    var text = document.getElementById('speedText');
    if (ms === null || ms < 0 || !badge || !text) {
        if (text) text.textContent = 'Load time unavailable in this browser';
        return;
    }

    var label, cls;
    if (ms < 100) {
        label = 'Blazing fast';
        cls = 'speed-fast';
    } else if (ms < 300) {
        label = 'Fast';
        cls = 'speed-fast';
    } else if (ms < 1000) {
        label = 'Moderate';
        cls = 'speed-moderate';
    } else {
        label = 'Slow';
        cls = 'speed-slow';
    }

    badge.classList.remove('speed-fast', 'speed-moderate', 'speed-slow');
    badge.classList.add(cls);
    text.textContent = 'Loaded in ' + ms + ' ms — ' + label;
}

if (document.readyState === 'complete') {
    // setTimeout: loadEventEnd is only finalized once the load event itself
    // has fully fired, including for a script running after it.
    setTimeout(reportLoadSpeed, 0);
} else {
    window.addEventListener('load', function () {
        setTimeout(reportLoadSpeed, 0);
    });
}

// Copies the document-root path shown on the page to the clipboard.
function copyPath() {
    var pathText = document.getElementById('docRootPath').innerText;
    var btn = document.getElementById('copyBtn');

    navigator.clipboard.writeText(pathText).then(function () {
        var originalText = btn.innerText;
        btn.innerText = 'Copied!';
        btn.style.backgroundColor = 'var(--success-color)';
        btn.style.color = 'white';
        btn.style.borderColor = 'var(--success-color)';

        setTimeout(function () {
            btn.innerText = originalText;
            btn.style.backgroundColor = 'var(--surface-color)';
            btn.style.color = 'var(--text-secondary)';
            btn.style.borderColor = 'var(--border-color)';
        }, 2000);
    }).catch(function (err) {
        console.error('Failed to copy text: ', err);
    });
}

var copyBtn = document.getElementById('copyBtn');
if (copyBtn) {
    copyBtn.addEventListener('click', copyPath);
}
