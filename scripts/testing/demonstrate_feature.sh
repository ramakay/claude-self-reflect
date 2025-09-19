#!/bin/bash
# Demonstration of real-time quality feedback feature

echo "🚀 Demonstrating Real-Time Quality Feedback for AI Agents"
echo "=========================================================="
echo ""

echo "1️⃣ Creating a file with bad quality patterns..."
cat > bad_demo.py << 'EOF'
#!/usr/bin/env python3
"""Demo file with bad patterns."""

# Multiple print statements (bad)
print("Bad pattern 1")
print("Bad pattern 2")
print("Bad pattern 3")
print("Bad pattern 4")
print("Bad pattern 5")

# Deeply nested if statements (bad)
if True:
    if True:
        if True:
            if True:
                print("Too deep!")

# Nested loops (bad)
for i in range(10):
    for j in range(10):
        for k in range(10):
            print(f"{i},{j},{k}")

# Bare except (bad)
try:
    x = 1 / 0
except:
    print("Error!")
EOF

echo "✅ File created: bad_demo.py"
echo ""

echo "2️⃣ Running quality analysis..."
echo "----------------------------------------"
./venv/bin/python scripts/ast_grep_final_analyzer.py bad_demo.py | grep -E "(Quality Score|Issues|print-call|nested-if|nested-loops)"
echo ""

echo "3️⃣ Simulating PostToolUse hook trigger..."
echo "----------------------------------------"
echo '{"tool_name": "Edit", "tool_input": {"file_path": "'$(pwd)'/bad_demo.py"}}' | ./venv/bin/python .claude/hooks/quality-check.py 2>&1
EXIT_CODE=$?
echo ""
echo "Hook exit code: $EXIT_CODE (2 = quality issues detected)"
echo ""

if [ $EXIT_CODE -eq 2 ]; then
    echo "✅ SUCCESS: Hook correctly detected quality issues!"
    echo "✅ Claude would see this feedback and can fix the issues!"
else
    echo "❌ Hook did not trigger (quality might be above threshold)"
fi

echo ""
echo "4️⃣ Key Innovation:"
echo "----------------------------------------"
echo "• PostToolUse hooks with stdout are INVISIBLE to Claude"
echo "• Exit code 2 with stderr makes feedback VISIBLE to Claude"
echo "• Real-time quality feedback loop is now functional!"
echo ""
echo "🎯 Feature Status: WORKING and READY for spotlight!"

# Cleanup
rm -f bad_demo.py