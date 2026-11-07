/* Stratum slider for the figure on this page; the page reads the same without it. */
(function () {
  var f = document.getElementById('strata-fig');
  if (!f) return;
  var c = f.querySelector('.strata-ctl'), r = c.querySelector('input'), o = c.querySelector('output');
  function n(side, l) {
    return [].filter.call(f.querySelectorAll('[data-s="' + side + '"]'), function (e) {
      return +e.getAttribute('data-t') >= l;
    }).length;
  }
  function show() {
    var l = +r.value;
    f.setAttribute('data-l', l);
    o.textContent = 'L = ' + l + ': ' + n('a', l) + ' of 32 lines of A, ' + n('b', l) + ' of 34 lines of B, ' + n('p', l) + ' the same line in both';
  }
  r.addEventListener('input', show);
  c.hidden = false;
  show();
})();
