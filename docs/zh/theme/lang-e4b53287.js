// zh book → EN book: same page one directory up.
(function () {
    var page = location.pathname.split("/").pop() || "index.html";
    var el = document.createElement("a");
    el.href = "../" + page;
    el.className = "lang-toggle";
    el.title = "Read in English";
    el.textContent = "EN";
    var bar = document.querySelector(".right-buttons");
    if (bar) {
        el.classList.add("icon-button");
        bar.prepend(el);
    }
})();
