// How a payment provider's fields format what is typed, as a hosted
// checkout's do, on every input as the keys arrive: a card number in
// groups (4-4-4-4, an Amex 4-6-5), an expiry as "MM / YY".
const cardFormat = {
  number(value) {
    const digits = value.replace(/\D/g, '').slice(0, 19);
    const groups = /^3[47]/.test(digits) ? [4, 6, 5] : [4, 4, 4, 4, 3];
    const out = [];
    let at = 0;
    for (const n of groups) {
      if (at >= digits.length) break;
      out.push(digits.slice(at, at + n));
      at += n;
    }
    return out.join(' ');
  },
  expiry(value) {
    const digits = value.replace(/\D/g, '').slice(0, 4);
    return digits.length > 2 ? digits.slice(0, 2) + ' / ' + digits.slice(2) : digits;
  },
};
const formatAs = (input, kind) =>
  input.addEventListener('input', () => { input.value = cardFormat[kind](input.value); });
