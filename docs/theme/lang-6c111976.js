// EN book → zh book: same page one directory down (/docs/zh/).
(function () {
    var page = location.pathname.split("/").pop() || "index.html";
    var el = document.createElement("a");
    el.href = "zh/" + page;
    el.className = "lang-toggle";
    el.title = "阅读中文版";
    el.textContent = "中文";
    var bar = document.querySelector(".right-buttons");
    if (bar) {
        el.classList.add("icon-button");
        bar.prepend(el);
    }
})();
