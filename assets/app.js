/* plugboard front-end glue. All server interaction stays htmx-driven; this
   file only reacts to htmx events: it surfaces failed actions as toasts,
   dismisses the confirm modal (backdrop click / Escape), keeps focus sane
   when a modal opens, and nudges the device detail page's live region to
   refresh right after a toggle. */
(function () {
  "use strict";

  function toast(message) {
    var region = document.getElementById("toasts");
    if (!region) return;
    var el = document.createElement("div");
    el.className = "toast error";
    var span = document.createElement("span");
    span.textContent = message;
    el.appendChild(span);
    region.appendChild(el);
  }

  function clearModal() {
    var modal = document.getElementById("modal");
    if (modal && modal.firstChild) modal.innerHTML = "";
  }

  document.addEventListener("DOMContentLoaded", function () {
    // Surface failed actions: htmx leaves non-2xx responses unswapped, so
    // without this an error would look like a dead button. The app's error
    // bodies are short plain-text reasons (always inserted via textContent,
    // never as markup); anything HTML-shaped or oversized collapses to the
    // status code.
    document.body.addEventListener("htmx:responseError", function (evt) {
      var xhr = evt.detail.xhr;
      var text = (xhr.responseText || "").trim();
      if (!text || text.charAt(0) === "<" || text.length > 300) {
        text = "request failed (" + xhr.status + ")";
      }
      toast(text);
    });
    document.body.addEventListener("htmx:sendError", function () {
      toast("network error: could not reach plugboard");
    });

    // Dismiss the confirm modal on backdrop click or Escape. Closing is
    // purely client-side state; the server holds nothing per-modal.
    document.body.addEventListener("click", function (evt) {
      if (evt.target.classList.contains("modal-backdrop")) clearModal();
    });
    document.addEventListener("keydown", function (evt) {
      if (evt.key === "Escape") clearModal();
    });

    // When a confirm modal swaps in, move focus to its Cancel button so
    // keyboard users land inside the dialog (Escape/Tab then work from it).
    var modal = document.getElementById("modal");
    if (modal) {
      new MutationObserver(function () {
        var cancel = modal.querySelector(".btn-cancel");
        if (cancel) cancel.focus();
      }).observe(modal, { childList: true });
    }

    // Toasts arrive as out-of-band swaps, and when the requesting element
    // was itself replaced by the same response (a card toggle swapping its
    // own card), htmx can skip initializing the inserted toast - leaving a
    // dead Undo button. htmx.process is a no-op on already-initialized
    // nodes, so processing every added toast is safe.
    var toasts = document.getElementById("toasts");
    if (toasts) {
      new MutationObserver(function (mutations) {
        mutations.forEach(function (m) {
          m.addedNodes.forEach(function (n) {
            if (n.nodeType === 1 && window.htmx) htmx.process(n);
          });
        });
      }).observe(toasts, { childList: true });
    }

    // A clicked toast dismisses itself - except on Undo: removing that
    // button's toast mid-request would detach the element, and its
    // htmx:afterRequest would no longer bubble to the body listener below.
    // The Undo toast leaves on its own timer instead.
    document.body.addEventListener("click", function (evt) {
      if (evt.target.closest(".undo")) return;
      var t = evt.target.closest(".toast");
      if (t) t.remove();
    });

    // SSE bursts re-send every card each poll tick, changed or not. Skip
    // swaps whose payload matches the element as rendered, so a no-op tick
    // never destroys focus, text selection, or an in-progress click. When
    // serialization differs the comparison just fails and the swap proceeds,
    // which is today's behavior.
    document.body.addEventListener("htmx:sseBeforeMessage", function (evt) {
      var el = evt.target;
      if (el && evt.detail && el.outerHTML === evt.detail.data) {
        evt.preventDefault();
      }
    });

    // Reveal a freshly swapped-in admin result (it can sit below the fold of
    // the form that produced it), and keep the console log pinned to its
    // newest entry like a real terminal.
    var reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
    document.body.addEventListener("htmx:afterSwap", function (evt) {
      var t = evt.detail.target;
      if (!t) return;
      if (t.id === "admin-result" && t.firstChild) {
        t.scrollIntoView({
          block: "nearest",
          behavior: reducedMotion.matches ? "auto" : "smooth",
        });
      }
      if (t.id === "console-log") {
        t.scrollTop = t.scrollHeight;
      }
    });

    // Chart hover: a crosshair plus the exact sample value and age, read
    // from the data-* attributes the server rendered alongside the SVG. A
    // gap sample honestly reads "no reading", never zero.
    function chartData(el) {
      if (el.plugboardChart !== undefined) return el.plugboardChart;
      try {
        el.plugboardChart = {
          w: JSON.parse(el.dataset.w),
          t: JSON.parse(el.dataset.t),
          cap: parseInt(el.dataset.cap, 10),
        };
      } catch (e) {
        el.plugboardChart = null;
      }
      return el.plugboardChart;
    }
    function fmtWatts(w) {
      return w >= 1000 ? (w / 1000).toFixed(2) + " kW" : w.toFixed(1) + " W";
    }
    function fmtAge(secs) {
      if (secs < 5) return "just now";
      if (secs < 90) return secs + "s ago";
      if (secs < 3600) return Math.round(secs / 60) + "m ago";
      return Math.floor(secs / 3600) + "h " + Math.round((secs % 3600) / 60) + "m ago";
    }
    var hoveredChart = null;
    document.body.addEventListener("pointermove", function (evt) {
      var chart = evt.target.closest ? evt.target.closest(".spark-chart") : null;
      if (hoveredChart && hoveredChart !== chart) {
        hoveredChart.classList.remove("hovered");
        hoveredChart = null;
      }
      if (!chart) return;
      var data = chartData(chart);
      if (!data || !data.t.length || data.cap < 2) return;
      var rect = chart.getBoundingClientRect();
      var frac = (evt.clientX - rect.left) / rect.width;
      var n = data.t.length;
      // Mirror the server's x mapping: sample i sits at (cap - n + i)/(cap - 1).
      var i = Math.round(frac * (data.cap - 1)) - (data.cap - n);
      i = Math.max(0, Math.min(n - 1, i));
      var xFrac = (data.cap - n + i) / (data.cap - 1);
      chart.classList.add("hovered");
      hoveredChart = chart;
      var cursor = chart.querySelector(".chart-cursor");
      var tip = chart.querySelector(".chart-tip");
      if (cursor) cursor.style.left = (xFrac * 100).toFixed(2) + "%";
      if (tip) {
        var w = data.w[i];
        var age = Math.max(0, Math.round(Date.now() / 1000 - data.t[i]));
        tip.textContent =
          (w === null ? "no reading" : fmtWatts(w)) + " · " + fmtAge(age);
        tip.style.left = Math.min(88, Math.max(12, xFrac * 100)).toFixed(2) + "%";
      }
    });
    document.body.addEventListener("pointerleave", function () {
      if (hoveredChart) {
        hoveredChart.classList.remove("hovered");
        hoveredChart = null;
      }
    });

    // The device detail page's toggle (and any control marked
    // refreshes-live, e.g. the update-check button) discards its direct
    // response (hx-swap="none"); refresh the live status region immediately
    // instead of waiting out the poll interval.
    document.body.addEventListener("htmx:afterRequest", function (evt) {
      var elt = evt.detail.elt;
      if (!elt || !elt.classList) return;
      if (
        !elt.classList.contains("device-toggle") &&
        !elt.classList.contains("refreshes-live")
      ) {
        return;
      }
      var live = document.getElementById("device-live");
      if (live && window.htmx) htmx.trigger(live, "refresh-live");
    });
  });
})();
