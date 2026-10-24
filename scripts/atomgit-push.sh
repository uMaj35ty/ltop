#!/usr/bin/env bash
# 镜像 master + 全部 tag 到 AtomGit(发布清单最后一步)。
#
# 镜像特有约定:AtomGit 的 README.md 是中文默认版(docs/README.zh-CN.md
# 内容,切换行指向 README.en.md),README.en.md 顶部切回 README.md——
# GitHub 侧根 README.md 是英文且无切换行。每次同步会覆盖这两个文件,
# 脚本末尾自动重建(commit 350429a/65fcdd8 约定的延续)。
#
# 用法: bash scripts/atomgit-push.sh [--force]
#   --force  首次对齐或镜像被特有提交分叉时使用(之后普通 push 即可)
# 鉴权: SSH(本机 id_ed25519 已注册,网关应答 Welcome to GitCode);
#       HTTPS 匿名只能读不能推。
set -euo pipefail
cd "$(dirname "$0")/.."
FORCE=${1:-}
REMOTE=${ATOMGIT_REMOTE:-atomgit}
git remote get-url "$REMOTE" >/dev/null 2>&1 || \
    git remote add "$REMOTE" git@atomgit.com:uMaj35ty/ltop.git

# 1) master + 全部 tag
if [ "$FORCE" = "--force" ]; then
    git push --force "$REMOTE" refs/heads/master:refs/heads/master
else
    git push "$REMOTE" refs/heads/master:refs/heads/master
fi
git push "$REMOTE" --tags

# 2) 重建镜像特有 zh README(同步覆盖后自动重做)
git fetch "$REMOTE" master -q
git checkout -q -B mirror-zh "refs/remotes/$REMOTE/master"
# 中文版:切换行指 README.en.md;资产路径从 docs/ 视角(../assets/)改根视角
sed -e 's|(\.\./README\.md)|(README.en.md)|' -e 's|\.\./assets/|assets/|g' \
    docs/README.zh-CN.md > README.md
# 英文版:顶部加切回 README.md 的切换行
{ echo "# ltop"; echo; echo "[简体中文](README.md) | English"; echo; cat README.md; } > README.en.md
git add README.md README.en.md
git commit -q -m "mirror: Chinese-first README (presentation only)" || true
git push "$REMOTE" refs/heads/mirror-zh:refs/heads/master
git checkout -q master
git branch -D mirror-zh
echo "AtomGit 镜像已更新: https://atomgit.com/uMaj35ty/ltop"
