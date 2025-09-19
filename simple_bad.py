#!/usr/bin/env python3
"""Simple test file with obvious quality issues."""

# Bad: print statements
print("This is bad")
print("Another bad print")

# Bad: bare except
try:
    x = 1 / 0
except:
    print("Error!")

# More bad patterns
for i in range(10):
    for j in range(10):
        print(f"Nested: {i}, {j}")