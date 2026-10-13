"""git sets GIT_DIR, GIT_INDEX_FILE and more for a pre-commit hook, and every git these tests run in a scratch folder would
then be the hook's repository: `git add` in a fixture staged its file in the real index, and "this folder is not in git"
was false. The tests run in a clean git environment."""
import os

for _name in [name for name in os.environ if name.startswith('GIT_')]:
    del os.environ[_name]
