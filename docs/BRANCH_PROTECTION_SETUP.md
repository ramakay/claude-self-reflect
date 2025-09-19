# Branch Protection Setup Guide

## Why Branch Protection?

Branch protection rules prevent direct pushes to the main branch and enforce our CI/CD workflow. This ensures:
- All changes go through PR review process
- CodeRabbit automated review runs on all changes
- CI/CD tests pass before merge
- No accidental direct pushes to main

## Manual Setup via GitHub UI

Since branch protection requires admin permissions, it needs to be configured manually:

### 1. Navigate to Settings
1. Go to https://github.com/ramakay/claude-self-reflect
2. Click on "Settings" tab
3. Select "Branches" from the left sidebar

### 2. Add Branch Protection Rule
1. Click "Add rule" button
2. Enter `main` in "Branch name pattern"

### 3. Configure Protection Settings

#### Required Settings:
- ✅ **Require a pull request before merging**
  - ✅ Dismiss stale pull request approvals when new commits are pushed
  - ⬜ Require review from CODEOWNERS (optional)
  - Required approvals: 0 (since we use CodeRabbit)

- ✅ **Require status checks to pass before merging**
  - ✅ Require branches to be up to date before merging
  - Select these status checks:
    - `CI/CD Pipeline`
    - `Security Scan`
    - `CodeRabbit` (if available)

- ✅ **Require conversation resolution before merging**

- ✅ **Include administrators** (recommended for consistency)

#### Optional Settings:
- ⬜ Require signed commits
- ⬜ Require linear history
- ✅ **Do not allow bypassing the above settings**

### 4. Save Changes
Click "Create" to enable branch protection

## GitHub CLI Commands (Admin Required)

If you have admin access, you can use these commands:

```bash
# Enable basic branch protection
gh api repos/ramakay/claude-self-reflect/branches/main/protection \
  -X PUT \
  -F enforce_admins=true \
  -F required_status_checks='{"strict":true,"contexts":["CI/CD Pipeline"]}' \
  -F required_pull_request_reviews='{"dismiss_stale_reviews":true,"required_approving_review_count":0}' \
  -F restrictions=null \
  -F allow_force_pushes=false \
  -F allow_deletions=false
```

## Git Hooks Alternative (Local Enforcement)

For local development, create a pre-push hook:

```bash
cat > .git/hooks/pre-push << 'EOF'
#!/bin/bash
# Prevent direct pushes to main branch

protected_branch='main'
current_branch=$(git symbolic-ref HEAD | sed -e 's,.*/\(.*\),\1,')

if [ "$current_branch" = "$protected_branch" ]; then
    echo "⛔ Direct push to main branch is not allowed!"
    echo "📝 Please create a feature branch and PR instead:"
    echo ""
    echo "  git checkout -b feature/your-feature"
    echo "  git push origin feature/your-feature"
    echo "  gh pr create"
    echo ""
    exit 1
fi
EOF

chmod +x .git/hooks/pre-push
```

## Workflow After Protection

Once enabled, the workflow becomes:

1. Create feature branch: `git checkout -b feature/description`
2. Make changes and commit
3. Push to origin: `git push origin feature/description`
4. Create PR: `gh pr create`
5. Wait for CodeRabbit review
6. Address feedback
7. Merge via GitHub UI or `gh pr merge`

## Updating Open-Source Maintainer Agent

The `open-source-maintainer` agent should be updated to:
1. Always create a feature branch
2. Never push directly to main
3. Always create a PR
4. Wait for CodeRabbit review before merge

This ensures our automated release process follows the same standards as manual development.