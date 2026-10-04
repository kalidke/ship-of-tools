// project-log site script: navigation, table of contents, math, and in-place page
// swaps. Hand-edited; the only thing to maintain is SITE_SECTIONS below.

function renderMath(root) {
  if (typeof renderMathInElement !== "function") return;
  renderMathInElement(root || document.body, {
    delimiters: [
      { left: "\\[", right: "\\]", display: true },
      { left: "\\(", right: "\\)", display: false },
    ],
    ignoredTags: ["script", "noscript", "style", "textarea", "pre", "code"],
    throwOnError: false,
  });
}

// Per-project: the site name shown in the header, and the navigation list.
// Add a page here when you add a file; the sidebar is built from this list only.
const SITE_NAME = "project-name";

const SITE_SECTIONS = [
  {
    title: "Start",
    pages: [
      ["index.html", "Front page"],
      ["status.html", "Where the work stands"],
    ],
  },
  {
    title: "Charter",
    pages: [
      ["charter/goals.html", "Goals and questions"],
    ],
  },
  {
    title: "Computational experiments",
    pages: [],
  },
  {
    title: "Wet-lab experiments",
    pages: [],
  },
  {
    title: "Journal",
    pages: [
      ["journal/index.html", "All entries"],
    ],
  },
  {
    title: "Reference",
    pages: [
      ["reference/glossary.html", "Terms and symbols"],
      ["reference/how-we-work.html", "How we work"],
    ],
  },
];

function siteRoot() {
  return document.body.dataset.root || ".";
}

function pagePath(path) {
  return `${siteRoot()}/${path}`;
}

function allPages() {
  return SITE_SECTIONS.flatMap((section) => section.pages);
}

// Absolute URL of the record root, fixed at first load. Page identities (data-page)
// are paths relative to this root.
let ROOT_URL = null;

function pageIdentity(href) {
  const url = new URL(href, document.baseURI);
  if (!ROOT_URL || !url.href.startsWith(ROOT_URL)) return null;
  const rel = url.href.slice(ROOT_URL.length).split("#")[0].split("?")[0];
  return rel.endsWith(".html") ? rel : null;
}

function renderHeader() {
  const target = document.querySelector("[data-site-header]");
  if (!target) return;
  const kind = document.body.dataset.kind || "Reference";
  target.className = "site-header";
  target.innerHTML = `
    <a class="site-brand" href="${pagePath("index.html")}" title="Front page">
      <strong>${SITE_NAME}</strong>
    </a>
    <span class="header-kind">${kind}</span>`;
}

const NAV_SCROLL_KEY = "project-log:nav-scroll";

// The sidebar is rendered once per page load and never again: every section is
// always open, every label is one line, and only the current-page highlight and
// the link targets change on navigation, so its layout is identical everywhere.
function renderNavigation() {
  const target = document.querySelector("[data-site-nav]");
  if (!target) return;
  target.classList.add("site-nav");
  target.setAttribute("aria-label", "Site navigation");
  target.innerHTML = SITE_SECTIONS.filter((section) => section.pages.length).map((section) => `
    <section class="nav-section">
      <h2 class="nav-section-title">${section.title}</h2>
      <ul class="nav-list">
        ${section.pages.map(([path, label]) => `
          <li><a class="nav-link" data-path="${path}" href="#" title="${label}">${label}</a></li>`).join("")}
      </ul>
    </section>`).join("");
  updateNavigation();
  try {
    const saved = sessionStorage.getItem(NAV_SCROLL_KEY);
    if (saved !== null) target.scrollTop = Number(saved);
  } catch (error) {}
  const remember = () => {
    try { sessionStorage.setItem(NAV_SCROLL_KEY, String(target.scrollTop)); } catch (error) {}
  };
  target.addEventListener("scroll", remember, { passive: true });
  window.addEventListener("pagehide", remember);
}

function updateNavigation() {
  const current = document.body.dataset.page;
  document.querySelectorAll(".nav-link[data-path]").forEach((link) => {
    const path = link.dataset.path;
    link.href = pagePath(path);
    if (path === current) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  });
}

function renderTableOfContents() {
  const target = document.querySelector("[data-page-toc]");
  if (!target) return;
  const headings = [...document.querySelectorAll("main h2[id], main h3[id]")];
  if (headings.length < 2) {
    target.hidden = true;
    target.innerHTML = "";
    return;
  }
  target.hidden = false;
  target.classList.add("page-toc");
  target.setAttribute("aria-label", "On this page");
  target.innerHTML = `
    <p class="toc-title">On this page</p>
    <ul class="toc-list">
      ${headings.map((heading) => `
        <li><a class="toc-link toc-${heading.tagName.toLowerCase()}"
          href="#${heading.id}">${heading.textContent}</a></li>`).join("")}
    </ul>`;
}

function renderSequence() {
  const target = document.querySelector("[data-page-sequence]");
  if (!target) return;
  const pages = allPages();
  const current = document.body.dataset.page;
  const index = pages.findIndex(([path]) => path === current);
  if (index < 0) return;
  const previous = pages[index - 1];
  const next = pages[index + 1];
  target.className = "page-sequence";
  target.innerHTML = `
    ${previous ? `<a class="sequence-link" href="${pagePath(previous[0])}">
      <span class="sequence-label">Previous</span>${previous[1]}</a>` : "<span></span>"}
    ${next ? `<a class="sequence-link next" href="${pagePath(next[0])}">
      <span class="sequence-label">Next</span>${next[1]}</a>` : "<span></span>"}`;
}

function renderFooter() {
  // The footer lives inside the content column, so the sticky sidebar's containing
  // block reaches the end of the document and is never pushed up by a trailing footer.
  const outer = document.querySelector("[data-site-footer]");
  if (outer) outer.remove();
  const main = document.querySelector("main.content");
  if (!main || main.querySelector(".site-footer")) return;
  const footer = document.createElement("footer");
  footer.className = "site-footer";
  footer.textContent = "Hand-written HTML, no build step · scripts generate figures and result rows, not these pages";
  main.appendChild(footer);
}

// Everything that belongs to the content column and changes from page to page.
function renderContent(root) {
  renderMath(root || document.querySelector("main.content"));
  renderHeader();
  updateNavigation();
  renderTableOfContents();
  renderSequence();
  renderFooter();
}

// In-place navigation: fetch the target page, swap only the content column, and
// update the address bar. The sidebar is untouched, so nothing blinks or moves.
// Falls back to a normal navigation when the page cannot be fetched (for example
// when the record is opened from the local file system).
async function swapPage(href, push) {
  const page = pageIdentity(href);
  if (!page) return false;
  let html;
  try {
    const response = await fetch(href, { credentials: "same-origin" });
    if (!response.ok) return false;
    html = await response.text();
  } catch (error) {
    return false;
  }
  const parsed = new DOMParser().parseFromString(html, "text/html");
  const incoming = parsed.querySelector("main.content");
  const outgoing = document.querySelector("main.content");
  if (!incoming || !outgoing) return false;
  if (push) history.pushState({ page }, "", href);
  document.title = parsed.title;
  document.body.dataset.page = page;
  document.body.dataset.root = parsed.body.dataset.root || ".";
  document.body.dataset.kind = parsed.body.dataset.kind || "";
  outgoing.replaceWith(document.adoptNode(incoming));
  renderContent();
  const hash = new URL(href, document.baseURI).hash;
  const anchor = hash ? document.getElementById(decodeURIComponent(hash.slice(1))) : null;
  if (anchor) anchor.scrollIntoView();
  else window.scrollTo({ top: 0, left: 0, behavior: "instant" });
  return true;
}

function interceptLinks() {
  document.addEventListener("click", (event) => {
    if (event.defaultPrevented || event.button !== 0) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const link = event.target.closest("a[href]");
    if (!link || link.target === "_blank" || link.hasAttribute("download")) return;
    const url = new URL(link.href, document.baseURI);
    if (url.origin !== location.origin) return;
    if (!pageIdentity(url.href)) return;
    const here = new URL(location.href);
    if (url.pathname === here.pathname && url.search === here.search) return; // same-page anchor
    event.preventDefault();
    swapPage(url.href, true).then((done) => {
      if (!done) location.href = url.href;
    });
  });
  window.addEventListener("popstate", () => {
    swapPage(location.href, false).then((done) => {
      if (!done) location.reload();
    });
  });
}

function setFavicon() {
  if (document.querySelector("link[rel='icon']")) return;
  const link = document.createElement("link");
  link.rel = "icon";
  link.href = "data:image/svg+xml," + encodeURIComponent(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><circle cx="5" cy="8" r="3.2" fill="#1d7f7a"/><circle cx="11" cy="8" r="3.2" fill="#d98b3a" fill-opacity="0.85"/></svg>');
  document.head.appendChild(link);
}

document.addEventListener("DOMContentLoaded", () => {
  ROOT_URL = new URL(`${siteRoot()}/`, document.baseURI).href;
  setFavicon();
  renderMath();
  renderHeader();
  renderNavigation();
  renderTableOfContents();
  renderSequence();
  renderFooter();
  interceptLinks();
});
