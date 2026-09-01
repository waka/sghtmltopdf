// Adds a language-switch link at the right end of the menu bar.
//
// mdbook has no built-in language switcher, and vendoring the whole theme (index.hbs)
// would mean chasing every mdbook upgrade, so we inject it with JS instead.
// (see docs/decisions 0064).
//
// `/` serves Japanese and `/en/` serves English (decision 0064, revision 4). We look at
// whether the current path contains `/en/` to build a link to the same page in the other language.
(function () {
  "use strict";

  // path_to_root is the "relative path from this page to the site root" that mdbook
  // embeds in every page (e.g. "../"). In the English edition the root is book/en/.
  var pathToRoot = document.querySelector("html").dataset.pathToRoot || "";

  var isEnglish = /(^|\/)en\//.test(window.location.pathname);
  var label = isEnglish ? "日本語" : "English";
  // English -> Japanese goes one level above the root; Japanese -> English goes to en/ under the root.
  var href = isEnglish ? pathToRoot + "../" : pathToRoot + "en/";

  var rightButtons = document.querySelector(".right-buttons");
  if (!rightButtons) {
    return;
  }

  var link = document.createElement("a");
  link.href = href;
  link.title = isEnglish ? "Read in Japanese" : "Read in English";
  link.className = "lang-switch";
  link.textContent = label;
  rightButtons.appendChild(link);
})();
