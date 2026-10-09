// The app fills exactly what's visible on a phone, and stays put.
//
// Phones move the page when the keyboard opens (an iPhone scrolls the whole
// page up to show the box being typed in, and often leaves it there when the
// keyboard closes): the top, with the logo, ends up off the screen and a black
// band shows at the bottom. So the page itself never scrolls (index.css pins
// it; only the chat and lists inside scroll), the app's height follows the
// visible area (`--app-height`, which is above the keyboard while it's open),
// and anything that shifts the page anyway is put back.

export function fitViewport() {
  const root = document.documentElement;
  const vv = window.visualViewport;
  let frame = 0;
  const fit = () => {
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => {
      const h = vv?.height ?? window.innerHeight;
      root.style.setProperty("--app-height", `${Math.round(h)}px`);
      // The keyboard is open: the safe area under it isn't the screen's edge any more.
      if (window.innerHeight - h > 120) root.dataset.keyboard = "";
      else delete root.dataset.keyboard;
      if (window.scrollY !== 0 || window.scrollX !== 0) window.scrollTo(0, 0);
    });
  };
  fit();
  vv?.addEventListener("resize", fit);
  vv?.addEventListener("scroll", fit);
  window.addEventListener("resize", fit);
  window.addEventListener("orientationchange", fit);
  window.addEventListener("scroll", fit, { passive: true });
  // The keyboard closing (the box loses focus): back to the whole screen.
  document.addEventListener("focusout", () => window.setTimeout(fit, 60));
  document.addEventListener("visibilitychange", fit);
}
