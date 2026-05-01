# Security TODO（紧急）

仓库历史中曾提交真实 secret，包括但不限于 SSH 密码、OpenAI / Anthropic API key、OAuth token、WandB token、Hugging Face token 等。

`.gitignore` 中新增 `.flux/` 只能阻止未来新文件继续被追踪；已经被 Git track 过的 `.flux/` 文件不会因为 `.gitignore` 自动消失，仍然会出现在历史和当前索引中，必须人工清理。

## 必须由人工执行的步骤

1. **立即旋转所有泄漏的 token**
   - SSH 密码 / 机器密码：在远端修改密码，或直接禁用密码登录并改用 SSH key
   - OpenAI API key：来源示例包括 `.flux/files/opencode.json`
   - Anthropic API key：来源示例包括 `.flux/files/opencode.json`
   - Codex OAuth token：来源示例包括 `.flux/files/codex_auth.json`
   - OpenCode auth token：来源示例包括 `.flux/files/opencode_auth.json`
   - WandB / Hugging Face / 任何 `.flux/blocks/variable.sh` 中出现过的 token

2. **从 Git 历史里清除**

```bash
git rm -r --cached .flux/
git commit -m "remove .flux/ tracked secrets"
# 之后清历史（不可逆，需团队对齐）：
pip install git-filter-repo
git filter-repo --path .flux --invert-paths
git push --force --all   # 协调好再做
```

3. **验证清除**

```bash
git log --all --full-history -- .flux/
```

应为空。

4. **未来防线**
   - 安装 secret 扫描钩子，例如 `gitleaks` 或 `trufflehog`
   - 把真实配置只放在 `.flux/`
   - 把可共享的结构、注释和默认值维护在 `.flux.example/`
   - 评估是否彻底移除基于密码的 SSH 登录
