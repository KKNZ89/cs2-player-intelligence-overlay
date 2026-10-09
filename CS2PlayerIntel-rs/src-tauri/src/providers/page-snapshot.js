(() => {
  const selectors = '[role="dialog"], [aria-modal="true"], [id*="cookie" i], [class*="cookie" i], [id*="consent" i], [class*="consent" i]';
  const consentRequired = [...document.querySelectorAll(selectors)].some(element => {
    const bounds = element.getBoundingClientRect();
    const style = getComputedStyle(element);
    if (!bounds.width || !bounds.height || bounds.bottom <= 0 || bounds.top >= innerHeight || bounds.right <= 0 || bounds.left >= innerWidth
      || style.display === 'none' || style.visibility !== 'visible') return false;
    if (!/\bcookies?\b|\bconsent\b/i.test(element.innerText || '')) return false;
    return [...element.querySelectorAll('button, a, [role="button"], input[type="button"], input[type="submit"]')]
      .some(control => !control.disabled && /\b(accept|agree|allow|deny|reject|decline|manage|preferences)\b/i.test(control.innerText || control.value || control.getAttribute('aria-label') || ''));
  });
  return {
    old: document.documentElement.hasAttribute('data-cs2intel-old'),
    text: document.body ? document.body.innerText : '',
    title: document.title || '',
    heading: (document.querySelector('h1') || {}).innerText || '',
    consent_required: consentRequired
  };
})()
