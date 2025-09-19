#!/usr/bin/env python3
"""
Test file to demonstrate real-time quality feedback.
This will trigger the PostToolUse hook.
"""

# Bad pattern: global variable
GLOBAL_CONFIG = {"debug": True}

def bad_function(data):
    """Function with multiple quality issues."""

    # Bad: print statements instead of logging
    print("Starting processing")
    print(f"Data: {data}")

    # Bad: deeply nested if statements
    if data:
        if len(data) > 0:
            if data[0]:
                if data[0] > 10:
                    print("Deep nesting!")

    # Bad: bare except
    try:
        result = data[0] / 0
    except:
        print("Error happened")
        pass

    # Bad: complex condition
    if data and len(data) > 5 and data[0] > 0 and data[-1] < 100 and sum(data) > 50:
        print("Complex condition!")

    return data

# Bad: module-level print
print("Module loaded")

# Adding more bad patterns to test
print("Another print statement")
DEBUG = True  # Another global

# Bad: nested loops
for i in range(10):
    for j in range(10):
        for k in range(10):
            print(f"Triple nested: {i},{j},{k}")