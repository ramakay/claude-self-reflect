#!/usr/bin/env python3
"""File with terrible quality to trigger the hook."""

# Tons of print statements
print("Bad 1")
print("Bad 2")
print("Bad 3")
print("Bad 4")
print("Bad 5")
print("Bad 6")
print("Bad 7")
print("Bad 8")
print("Bad 9")
print("Bad 10")

# Deep nesting
if True:
    if True:
        if True:
            if True:
                if True:
                    print("Too deep!")

# Complex condition
if True and True and True and True and True and True:
    print("Too complex!")

# Triple nested loops
for i in range(10):
    for j in range(10):
        for k in range(10):
            print(f"{i},{j},{k}")

# Bare except everywhere
try:
    x = 1
except:
    pass

try:
    y = 2
except:
    pass

try:
    z = 3
except:
    pass

# Even more bad code
print("This file has terrible quality and should trigger feedback!")

# Adding more problematic patterns
GLOBAL_VAR = "bad"  # Global variable
print(f"Debug: {GLOBAL_VAR}")  # Debug print
print("Another one")  # More prints
print("Even more prints!")  # Testing hook visibility
print("Final test of improved formatting")  # Better feedback