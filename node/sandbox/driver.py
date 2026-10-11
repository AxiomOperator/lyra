"""lyra's sandbox browser driver: one JSON request per line in, one answer
per line out ({"ok": true, "value": ...} or {"ok": false, "error": ...}).

Actions: open {url} · read · click {n} · fill {n, text} · select {n, value}
· submit {n?} · back · screenshot · close. With "shot": true, a step to
another page also answers with its picture. A read numbers the page's links,
buttons and fields and says which send a form, take payment details or
upload a file; lyra decides (in Rust) what may run before asking for it.
"""

import base64
import json
import sys
import time

from playwright.sync_api import sync_playwright

MARK = r"""() => {
  const out = []; let n = 0;
  const shown = (el) => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el); return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'; };
  document.querySelectorAll('[data-lyra-n]').forEach((e) => e.removeAttribute('data-lyra-n'));
  for (const el of document.querySelectorAll('a[href], button, input, textarea, select, [role=button], [role=link]')) {
    const type = (el.getAttribute('type') || '').toLowerCase();
    if (type === 'hidden' || !shown(el)) continue;
    n += 1; el.setAttribute('data-lyra-n', String(n));
    const tag = el.tagName.toLowerCase(); const role = el.getAttribute('role');
    const kind = tag === 'a' || role === 'link' ? 'link'
      : tag === 'button' || role === 'button' || ['submit', 'button', 'reset', 'image'].includes(type) ? 'button'
      : tag === 'select' ? 'select' : ['checkbox', 'radio'].includes(type) ? type : 'field';
    const label = (el.labels && el.labels[0] && el.labels[0].innerText) || el.getAttribute('aria-label') || el.getAttribute('placeholder') || el.innerText || el.value || el.getAttribute('name') || el.getAttribute('title') || '';
    const auto = (el.getAttribute('autocomplete') || '').toLowerCase();
    const names = ((el.getAttribute('name') || '') + ' ' + (el.id || '')).toLowerCase();
    const payment = auto.startsWith('cc-') || /card.?num|cvv|cvc|ccnum|cc.?exp|security.?code/.test(names);
    const form = el.form || el.closest('form');
    const submits = (kind === 'button' && !!form && type !== 'button' && type !== 'reset') || type === 'submit' || type === 'image';
    const item = { n, kind, label: label.trim().replace(/\s+/g, ' ').slice(0, 80) };
    if (el.getAttribute('name')) item.name = el.getAttribute('name');
    if (type) item.type = type;
    if (tag === 'a') item.href = el.href;
    if (submits) item.submits = true;
    if (payment) item.payment = true;
    if (type === 'file') item.file = true;
    if (type === 'password') item.password = true;
    if (kind === 'field' && type !== 'password' && el.value) item.value = el.value.slice(0, 80);
    out.push(item);
    if (n >= 150) break;
  }
  const text = document.body ? document.body.innerText.replace(/\n{3,}/g, '\n\n').slice(0, 8000) : '';
  return { elements: out, text };
}"""


def main():
    with sync_playwright() as p:
        # The container is the sandbox: Chromium's own needs privileges it doesn't get here.
        browser = p.chromium.launch(args=["--no-sandbox", "--disable-dev-shm-usage"], chromium_sandbox=False)
        context = browser.new_context(viewport={"width": 1280, "height": 900}, accept_downloads=False)
        page = context.new_page()

        def settle():
            try:
                page.wait_for_load_state("domcontentloaded", timeout=15000)
            except Exception:
                pass
            time.sleep(0.4)

        def read():
            info = page.evaluate(MARK)
            return {"url": page.url, "title": page.title(), **info}

        def shot():
            return "data:image/jpeg;base64," + base64.b64encode(page.screenshot(type="jpeg", quality=45)).decode()

        def element(args):
            return page.locator('[data-lyra-n="%d"]' % int(args["n"])).first

        print(json.dumps({"ok": True, "value": {"ready": True}}), flush=True)
        for line in sys.stdin:
            try:
                req = json.loads(line)
                action, args = req.get("action"), req.get("args") or {}
                if action == "open":
                    page.goto(args["url"], wait_until="domcontentloaded", timeout=30000)
                    settle()
                    out = read()
                elif action == "read":
                    out = read()
                elif action == "click":
                    element(args).click(timeout=10000)
                    settle()
                    out = read()
                elif action == "fill":
                    element(args).fill(str(args.get("text", "")), timeout=10000)
                    out = {"filled": args["n"]}
                elif action == "select":
                    element(args).select_option(str(args.get("value", "")), timeout=10000)
                    out = {"selected": args["n"]}
                elif action == "submit":
                    if args.get("n"):
                        element(args).click(timeout=10000)
                    else:
                        page.evaluate("() => { const f = document.querySelector('form'); if (f) f.requestSubmit(); }")
                    settle()
                    out = read()
                elif action == "back":
                    page.go_back(timeout=15000)
                    settle()
                    out = read()
                elif action == "screenshot":
                    out = {"url": page.url, "title": page.title(), "shot": shot()}
                elif action == "close":
                    print(json.dumps({"ok": True, "value": {"closed": True}}), flush=True)
                    break
                else:
                    raise ValueError("unknown action %r" % action)
                # A step to another page: its picture too, when asked (for the person, not the model).
                if args.get("shot") and action in ("open", "click", "submit", "back") and isinstance(out, dict):
                    out["shot"] = shot()
                answer = {"ok": True, "value": out}
            except Exception as e:  # the page's trouble, told back
                answer = {"ok": False, "error": str(e).splitlines()[0][:500] if str(e) else type(e).__name__}
            print(json.dumps(answer), flush=True)
        browser.close()


if __name__ == "__main__":
    main()
