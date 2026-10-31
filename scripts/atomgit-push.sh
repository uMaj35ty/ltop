#!/usr/bin/env bash
# 镜像 master + 全部 tag 到 AtomGit(发布清单最后一步)。
#
# 镜像特有约定:AtomGit 的 README.md 是中文默认版(docs/README.zh-CN.md
# 内容,切换行指向 README.en.md),README.en.md 顶部切回 English——
# GitHub 侧根 README.md 是英文且无切换行。同步会在临时 worktree 里重建
# 这两个文件并追加提交,绝不触碰主检出(曾因直接改工作树把 zh README
# 泄漏进 GitHub master)。
#
# 用法: bash scripts/atomgit-push.sh [--force]
#   --force  常规即需:镜像 master 总比本地多一个 README 提交(非快进)
# 鉴权: SSH(本机 id_ed25519 已注册);HTTPS 匿名只能读不能推。
set -euo pipefail
cd "$(dirname "$0")/.."
FORCE=${1:-}
REMOTE=${ATOMGIT_REMOTE:-atomgit}
WT=/tmp/atomgit-mirror
git remote get-url "$REMOTE" >/dev/null 2>&1 || \
    git remote add "$REMOTE" git@atomgit.com:uMaj35ty/ltop.git

# 1) master + 全部 tag(镜像 master 带特有提交,常规即需 --force)
if [ "$FORCE" = "--force" ]; then
    git push --force "$REMOTE" refs/heads/master:refs/heads/master
else
    git push "$REMOTE" refs/heads/master:refs/heads/master
fi
git push "$REMOTE" --tags

# 2) 临时 worktree 里重建镜像特有 zh README 并追加提交
git fetch "$REMOTE" master -q
git worktree add --detach "$WT" "refs/remotes/$REMOTE/master" >/dev/null 2>&1
(
    cd "$WT"
    sed -e 's|(\.\./README\.md)|(README.en.md)|' -e 's|\.\./assets/|assets/|g' \
        docs/README.zh-CN.md > README.md
    { echo "# ltop"; echo; echo "[简体中文](README.md) | English"; echo; \
      cat README.md; } > README.en.md
    git add README.md README.en.md
    git commit -q -m "mirror: Chinese-first README (presentation only)" || true
    git push "$REMOTE" HEAD:refs/heads/master
)
git worktree remove --force "$WT"
echo "AtomGit 镜像已更新: https://atomgit.com/uMaj35ty/ltop"
