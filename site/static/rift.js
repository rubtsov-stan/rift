/* Animates the page toward the state the HTML already shows. */
(function () {
  var d = document, root = d.documentElement;
  var still = !/\banim\b/.test(root.className);
  function $(s, c) { return [].slice.call((c || d).querySelectorAll(s)); }

  /* theme: dark until a visitor picks one */
  var tb = $('[data-theme-toggle]');
  function cur() { return root.getAttribute('data-theme') || 'dark'; }
  function label() {
    tb.forEach(function (b) {
      var n = cur() === 'dark' ? 'light' : 'dark';
      b.textContent = n === 'light' ? 'Light' : 'Dark';
      b.setAttribute('aria-label', 'Switch to the ' + n + ' theme');
    });
  }
  tb.forEach(function (b) {
    b.hidden = false;
    b.onclick = function () {
      var n = cur() === 'dark' ? 'light' : 'dark';
      root.setAttribute('data-theme', n);
      try { localStorage.setItem('rift-theme', n); } catch (e) {}
      label();
    };
  });
  label();

  /* header shrinks, the fault line opens with the scroll position */
  var top = d.querySelector('.top'), tick = 0;
  function onScroll() {
    tick = 0;
    var y = window.pageYOffset;
    if (top) top.classList.toggle('small', y > 40);
    if (!still) {
      var m = root.scrollHeight - window.innerHeight;
      root.style.setProperty('--split', m > 0 ? Math.min(1, y / Math.min(m, 1800)).toFixed(3) : 0);
    }
  }
  window.addEventListener('scroll', function () { if (!tick) tick = requestAnimationFrame(onScroll); }, { passive: true });
  onScroll();

  /* numbers count up to the value already in the HTML */
  function count(el) {
    var t = el.textContent, m = t.match(/\d[\d,]*\.?\d*/);
    if (!m) return;
    var s = m[0], to = parseFloat(s.replace(/,/g, '')), dec = (s.split('.')[1] || '').length;
    var pre = t.slice(0, m.index), post = t.slice(m.index + s.length);
    var delay = +el.getAttribute('data-delay') || 0, dur = 1100;
    setTimeout(function () {
      var t0 = performance.now();
      (function f(n) {
        var p = Math.min(1, (n - t0) / dur), e = 1 - Math.pow(1 - p, 3);
        el.textContent = pre + (to * e).toFixed(dec) + post;
        if (p < 1) requestAnimationFrame(f); else el.textContent = t;
      })(t0);
    }, delay);
  }

  /* the terminal types its command, then prints the output line by line */
  function type(el) {
    var c = el.querySelector('.cmd'), full = c.textContent, L = $('.ln', el), i = 0, j = 0;
    el.classList.add('run');
    c.innerHTML = '<span></span><span class="ghost"></span>';
    var a = c.firstChild, g = c.lastChild;
    (function k() {
      i++;
      a.textContent = full.slice(0, i);
      g.textContent = full.slice(i);
      if (i < full.length) setTimeout(k, 34 + Math.random() * 44); else setTimeout(rev, 380);
    })();
    function rev() {
      if (j < L.length) { L[j++].classList.add('on'); setTimeout(rev, 46); }
    }
  }

  if (!still && 'IntersectionObserver' in window) {
    var io = new IntersectionObserver(function (es) {
      es.forEach(function (e) {
        if (!e.isIntersecting) return;
        io.unobserve(e.target);
        e.target.classList.add('in');
        $('[data-count]', e.target).forEach(count);
        if (e.target.hasAttribute('data-term')) type(e.target);
      });
    }, { threshold: 0.25 });
    $('[data-reveal],[data-term]').forEach(function (el) { io.observe(el); });
  }

  /* copy buttons */
  $('[data-copy]').forEach(function (b) {
    b.hidden = false;
    b.onclick = function () {
      var t = b.parentNode.querySelector('pre').textContent.trim();
      var done = function () {
        b.textContent = 'Copied';
        setTimeout(function () { b.textContent = 'Copy'; }, 1600);
      };
      var old = function () {
        var a = d.createElement('textarea');
        a.value = t; d.body.appendChild(a); a.select();
        try { d.execCommand('copy'); done(); } catch (e) {}
        a.remove();
      };
      if (navigator.clipboard) navigator.clipboard.writeText(t).then(done, old); else old();
    };
  });

  /* star count: shown only at or above the threshold; the built page stays as it is if the request fails */
  try {
    var slot = d.querySelector('[data-star-min]');
    if (slot) {
      fetch('https://api.github.com/repos/' + slot.getAttribute('data-repo'))
        .then(function (r) { return r.ok ? r.json() : {}; })
        .then(function (j) {
          if (typeof j.stargazers_count === 'number' && j.stargazers_count >= +slot.getAttribute('data-star-min')) {
            slot.textContent = j.stargazers_count;
            slot.setAttribute('aria-label', j.stargazers_count + ' stars');
          }
        })
        .catch(function () {});
    }
  } catch (e) {}
})();
