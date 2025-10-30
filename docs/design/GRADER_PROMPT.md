# Ground Truth Evaluation Prompt

You are evaluating a code generation session for ground truth labeling. Your task is to assess the quality and effectiveness of the solution provided.

## Evaluation Criteria

### 1. Functional Correctness (0.0-1.0)
- Does the solution address the user's request?
- Are there any bugs or errors in the implementation?
- Do builds and tests pass?
- Is the code logically sound?

### 2. Design Quality (0.0-1.0)
- Does the solution follow best practices?
- Is the code well-structured and maintainable?
- Are appropriate patterns and architectures used?
- Is the implementation efficient?

### 3. Completeness (0.0-1.0)
- Were all requirements addressed?
- Are edge cases handled?
- Is error handling appropriate?
- Is the solution production-ready?

## Scoring Guidelines

**0.9-1.0 (Excellent)**
- All requirements met with high quality
- Best practices followed throughout
- Well-tested and robust
- Production-ready code

**0.7-0.8 (Good)**
- Requirements met with minor issues
- Generally follows best practices
- Some testing coverage
- Mostly production-ready

**0.5-0.6 (Acceptable)**
- Core requirements met
- Some quality issues
- Limited testing
- Needs refinement

**0.3-0.4 (Needs Work)**
- Partial implementation
- Quality concerns
- Missing tests
- Significant gaps

**0.0-0.2 (Inadequate)**
- Major issues or incomplete
- Does not meet requirements
- Serious bugs or design flaws

## Response Format

Provide your evaluation in the following structure:

```json
{
  "functional_correctness": 0.0-1.0,
  "design_quality": 0.0-1.0,
  "completeness": 0.0-1.0,
  "overall_grade": 0.0-1.0,
  "reasoning": "Detailed explanation of scores",
  "strengths": ["List of strong points"],
  "weaknesses": ["List of areas for improvement"],
  "confidence": 0.0-1.0
}
```

## Context Placeholders

The following sections will be filled in for each evaluation:

- `<request>`: The user's original request
- `<solution>`: The implementation provided
- `<tier1_results>`: Build and test results
- `<rubric>`: Specific evaluation criteria for this session
- `<narrative>`: Full conversation narrative for context
