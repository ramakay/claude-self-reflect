#!/usr/bin/env python3
"""
Demonstration file with intentional quality issues to trigger AST-GREP patterns.
This will show the real-time quality feedback feature in action.
"""

import os
import sys

# Global variable (bad pattern)
global_counter = 0
DEBUG_MODE = True

def process_data(data):
    """Function with multiple quality issues."""

    # Issue 1: Print statements instead of logging
    print("Starting processing")
    print(f"Data length: {len(data)}")

    # Issue 2: Deeply nested if statements (complexity pattern)
    if data:
        print("Data exists")
        if len(data) > 0:
            print("Data has items")
            if isinstance(data, list):
                print("Data is a list")
                if data[0] is not None:
                    print("First item is not None")
                    # Even deeper nesting
                    if data[0] > 0:
                        result = data[0] * 2

    # Issue 3: Complex condition (4+ parts)
    if data and len(data) > 5 and isinstance(data, list) and data[0] > 0 and data[-1] < 100:
        print("Complex validation passed")

    # Issue 4: Nested loops (performance risk)
    for i in range(len(data)):
        print(f"Processing item {i}")
        for j in range(len(data)):
            print(f"Comparing {i} with {j}")
            if data[i] == data[j]:
                print("Match found!")

    # Issue 5: Bare except clause
    try:
        result = data[0] / data[1]
        file = open('output.txt', 'w')  # Issue 6: Sync file operation
        file.write(str(result))
    except:  # Bad: bare except
        print("Error occurred")  # Bad: print instead of logger
        pass  # Bad: silent failure

    # Issue 7: Multiple elif branches (complexity)
    if data[0] == 1:
        return "one"
    elif data[0] == 2:
        return "two"
    elif data[0] == 3:
        return "three"
    elif data[0] == 4:
        return "four"
    elif data[0] == 5:
        return "five"
    elif data[0] == 6:
        return "six"
    elif data[0] == 7:
        return "seven"
    else:
        return "other"

    # Issue 8: Long function (this function has way too many lines)
    x = 1
    y = 2
    z = 3
    a = 4
    b = 5
    c = 6
    d = 7
    e = 8
    f = 9
    g = 10

    # Issue 9: Using global variable
    global global_counter
    global_counter += 1

    # Issue 10: Debug code left in
    if DEBUG_MODE:
        print("Debug: counter is", global_counter)

    return result


# Issue 11: Module-level print
print("Module loaded")
print("Another bad print statement")  # Adding more bad patterns

# Issue 12: Another complex nested structure
class ComplexClass:
    def complex_method(self, param1, param2, param3, param4, param5):  # Too many parameters
        # Triple nested structure
        if param1:
            for item in param2:
                if item > 0:
                    for sub in param3:
                        if sub == item:
                            print(f"Match: {sub}")


# This file should trigger multiple AST-GREP patterns:
# - print-call (multiple times)
# - nested-if-depth-3
# - complex-condition
# - nested-loops
# - broad-except / bare-except
# - sync-open (file operations)
# - multiple-elif
# - long-function
# - global-var
# - debug-print

# Adding another bad pattern to test real-time feedback
print("This should trigger immediate quality feedback!")