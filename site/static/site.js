// Small touches for the k-pdf website. Everything works without it.
(function () {
  "use strict";

  // Which system the visitor is on, to offer the right download first.
  function detectOS() {
    var p = ((navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "").toLowerCase();
    var ua = navigator.userAgent.toLowerCase();
    if (/win/.test(p) || /windows/.test(ua)) return "windows";
    if (/mac/.test(p) || /mac os/.test(ua)) return /iphone|ipad/.test(ua) ? null : "macos";
    if (/linux|x11/.test(p) && !/android/.test(ua)) return "linux";
    return null;
  }
  var names = { windows: "Windows", macos: "macOS", linux: "Linux" };
  var os = detectOS();

  // Highlight the current page in the header.
  var page = document.body.getAttribute("data-page");
  document.querySelectorAll(".site-nav [data-nav]").forEach(function (a) {
    if (a.getAttribute("data-nav") === page) a.classList.add("active");
  });

  // The main download button points straight at the file for this system.
  var main = document.getElementById("download-main");
  if (main && os) {
    var files = {};
    try { files = JSON.parse(main.getAttribute("data-files") || "{}"); } catch (e) { /* keep the default link */ }
    if (files[os]) {
      main.href = files[os];
      var label = main.querySelector("[data-label]");
      if (label) label.textContent = "Download for " + names[os];
    }
  }
  document.querySelectorAll('.download-card[data-os="' + os + '"]').forEach(function (c) { c.classList.add("current"); });

  // Install instructions: one tab per system, starting with the visitor's.
  var tabs = document.querySelectorAll(".os-tabs [role=tab]");
  function selectTab(key) {
    tabs.forEach(function (t) {
      var on = t.getAttribute("data-os") === key;
      t.setAttribute("aria-selected", on ? "true" : "false");
      t.tabIndex = on ? 0 : -1;
      var panel = document.getElementById(t.getAttribute("aria-controls"));
      if (panel) panel.hidden = !on;
    });
  }
  if (tabs.length) {
    tabs.forEach(function (t) {
      t.addEventListener("click", function () { selectTab(t.getAttribute("data-os")); });
    });
    selectTab(os && document.querySelector('.os-tabs [data-os="' + os + '"]') ? os : tabs[0].getAttribute("data-os"));
  }

  // Click a screenshot to see it full size.
  var box = document.querySelector(".lightbox");
  if (box && box.showModal) {
    var big = box.querySelector("img");
    document.addEventListener("click", function (e) {
      var img = e.target.closest && e.target.closest(".shot img, .hero-shot img");
      if (!img) return;
      big.src = img.currentSrc || img.src;
      big.alt = img.alt;
      box.showModal();
    });
    box.addEventListener("click", function () { box.close(); });
  }

  // Guide: mark the section being read in the table of contents.
  var toc = document.querySelector(".toc");
  if (toc) {
    var toggle = toc.querySelector(".toc-toggle");
    if (toggle) toggle.addEventListener("click", function () {
      var open = toc.classList.toggle("open");
      toggle.setAttribute("aria-expanded", open ? "true" : "false");
    });
    var links = {};
    toc.querySelectorAll("a[href^='#']").forEach(function (a) { links[a.getAttribute("href").slice(1)] = a; });
    if ("IntersectionObserver" in window) {
      var current = null;
      var observer = new IntersectionObserver(function (entries) {
        entries.forEach(function (entry) {
          if (!entry.isIntersecting) return;
          var a = links[entry.target.id];
          if (!a) return;
          if (current) current.classList.remove("active");
          a.classList.add("active");
          current = a;
        });
      }, { rootMargin: "-80px 0px -70% 0px" });
      document.querySelectorAll(".guide section[id]").forEach(function (s) { observer.observe(s); });
    }
  }
})();
