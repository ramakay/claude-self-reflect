#!/usr/bin/env python3
"""
Spike: Lazy Chunking with Agent Skills

Proves that we can use Anthropic's Agent Skills to analyze conversations
and generate problem-solution narratives for semantic search.

This demonstrates:
1. Upload custom "conversation-analyzer" Skill
2. Extract structured data from JSONL (fast, no LLM)
3. Use Skill with Claude Sonnet 4.5 to generate narrative
4. Track cost and timing
"""

import os
import sys
import json
import time
from pathlib import Path
from dotenv import load_dotenv

# Load environment
load_dotenv()

try:
    import anthropic
    from anthropic.lib import files_from_dir
except ImportError:
    print("Installing anthropic SDK...")
    os.system("pip install -q anthropic python-dotenv")
    import anthropic
    from anthropic.lib import files_from_dir


class LazyChunkingSpike:
    def __init__(self, api_key: str):
        self.client = anthropic.Anthropic(api_key=api_key)
        self.skill_id = None
        self.timing = {}
        self.costs = {}

    def upload_skill(self, skill_dir: Path):
        """Upload the conversation-analyzer Skill."""
        print("\n📤 Uploading conversation-analyzer Skill...")
        start = time.time()

        try:
            # Check if skill already exists
            skills = self.client.beta.skills.list(
                source="custom",
                betas=["skills-2025-10-02"]
            )

            existing = next(
                (s for s in skills.data if s.display_title == "Conversation Analyzer"),
                None
            )

            if existing:
                print(f"✅ Skill already exists: {existing.id}")
                self.skill_id = existing.id
            else:
                # Upload new skill
                skill = self.client.beta.skills.create(
                    display_title="Conversation Analyzer",
                    files=files_from_dir(str(skill_dir)),
                    betas=["skills-2025-10-02"]
                )
                self.skill_id = skill.id
                print(f"✅ Skill uploaded: {skill.id}")

            self.timing['skill_upload'] = time.time() - start
            return True

        except Exception as e:
            print(f"❌ Skill upload failed: {e}")
            return False

    def analyze_conversation(self, jsonl_path: Path):
        """Analyze conversation using the Skill."""
        print(f"\n🔍 Analyzing conversation: {jsonl_path.name}")

        # Phase 1: Extract structured data (Python-only, fast)
        print("\n  Phase 1: Extracting structured data...")
        start = time.time()

        # Use the Skill's extraction script
        extraction_script = Path(__file__).parent / "conversation-analyzer" / "extract_structured.py"
        import subprocess
        result = subprocess.run(
            [sys.executable, str(extraction_script), str(jsonl_path)],
            capture_output=True,
            text=True
        )

        if result.returncode != 0:
            print(f"❌ Extraction failed: {result.stderr}")
            return None

        structured_data = json.loads(result.stdout)
        self.timing['extraction'] = time.time() - start
        print(f"  ✅ Extracted in {self.timing['extraction']:.2f}s")
        print(f"     - {structured_data['total_messages']} messages")
        print(f"     - {len(structured_data['files']['edited'])} files edited")
        print(f"     - {len(structured_data['errors'])} errors found")

        # Phase 2: LLM analysis using Skill
        print("\n  Phase 2: Generating narrative with Claude Sonnet 4.5...")
        start = time.time()

        # Prepare the request
        messages = [{
            "role": "user",
            "content": f"""Analyze this conversation and generate a problem-solution narrative.

Conversation data:
```json
{json.dumps(structured_data, indent=2)}
```

Follow the SKILL.md instructions to produce a structured markdown narrative."""
        }]

        try:
            response = self.client.beta.messages.create(
                model="claude-sonnet-4-5-20250929",
                max_tokens=4096,
                betas=["code-execution-2025-08-25", "skills-2025-10-02"],
                container={
                    "skills": [{
                        "type": "custom",
                        "skill_id": self.skill_id,
                        "version": "latest"
                    }]
                },
                messages=messages,
                tools=[{
                    "type": "code_execution_20250825",
                    "name": "code_execution"
                }]
            )

            self.timing['llm_analysis'] = time.time() - start

            # Calculate cost
            input_tokens = response.usage.input_tokens
            output_tokens = response.usage.output_tokens

            # Sonnet 4.5 pricing: $3/1M input, $15/1M output
            input_cost = (input_tokens / 1_000_000) * 3.00
            output_cost = (output_tokens / 1_000_000) * 15.00
            total_cost = input_cost + output_cost

            self.costs['analysis'] = total_cost

            print(f"  ✅ Analysis completed in {self.timing['llm_analysis']:.2f}s")
            print(f"     - Input tokens: {input_tokens:,}")
            print(f"     - Output tokens: {output_tokens:,}")
            print(f"     - Cost: ${total_cost:.4f}")

            # Extract narrative from response
            narrative = ""
            for block in response.content:
                if hasattr(block, 'text'):
                    narrative += block.text

            return {
                'structured_data': structured_data,
                'narrative': narrative,
                'usage': response.usage,
                'cost': total_cost
            }

        except Exception as e:
            print(f"❌ LLM analysis failed: {e}")
            return None

    def display_results(self, result: dict):
        """Display formatted results."""
        print("\n" + "="*80)
        print("📊 ANALYSIS RESULTS")
        print("="*80)

        print("\n📝 Generated Narrative:\n")
        print(result['narrative'])

        print("\n" + "="*80)
        print("⏱️  TIMING BREAKDOWN")
        print("="*80)
        print(f"  Skill upload:    {self.timing.get('skill_upload', 0):.2f}s")
        print(f"  Data extraction: {self.timing.get('extraction', 0):.2f}s")
        print(f"  LLM analysis:    {self.timing.get('llm_analysis', 0):.2f}s")
        print(f"  TOTAL:           {sum(self.timing.values()):.2f}s")

        print("\n" + "="*80)
        print("💰 COST ANALYSIS")
        print("="*80)
        print(f"  This conversation: ${result['cost']:.4f}")
        print(f"  At 3,200 convos/year: ${result['cost'] * 3200:.2f}/year")

        # Extrapolate with prompt caching (90% savings)
        cached_cost = result['cost'] * 0.1
        print(f"\n  With prompt caching:")
        print(f"    First analysis: ${result['cost']:.4f}")
        print(f"    Cached (90% off): ${cached_cost:.4f}")
        print(f"    At 3,200 convos/year (90% cached): ${(result['cost'] * 320 + cached_cost * 2880):.2f}/year")


def find_sample_conversation() -> Path:
    """Find a sample conversation JSONL file."""
    projects_dir = Path.home() / ".claude" / "projects"

    if not projects_dir.exists():
        print(f"❌ Projects directory not found: {projects_dir}")
        return None

    # Look for JSONL files
    for project_dir in projects_dir.iterdir():
        if not project_dir.is_dir():
            continue

        for jsonl in project_dir.glob("*.jsonl"):
            # Skip very large files (>1MB)
            if jsonl.stat().st_size > 1_000_000:
                continue
            return jsonl

    print("❌ No suitable conversation JSONL found")
    return None


def main():
    print("🚀 Lazy Chunking Spike with Agent Skills")
    print("="*80)

    # Get API key
    api_key = os.getenv("ANTHROPIC_API_KEY")
    if not api_key:
        print("\n⚠️  ANTHROPIC_API_KEY not found in .env")
        api_key = input("Enter your Anthropic API key: ").strip()
        if not api_key:
            print("❌ API key required")
            return

    # Initialize spike
    spike = LazyChunkingSpike(api_key)

    # Upload Skill
    skill_dir = Path(__file__).parent / "conversation-analyzer"
    if not skill_dir.exists():
        print(f"❌ Skill directory not found: {skill_dir}")
        return

    if not spike.upload_skill(skill_dir):
        return

    # Find sample conversation
    sample_jsonl = find_sample_conversation()
    if not sample_jsonl:
        print("\n💡 Please provide path to a conversation JSONL:")
        jsonl_path = input("Path: ").strip()
        sample_jsonl = Path(jsonl_path)

        if not sample_jsonl.exists():
            print(f"❌ File not found: {sample_jsonl}")
            return

    # Analyze conversation
    result = spike.analyze_conversation(sample_jsonl)
    if not result:
        return

    # Display results
    spike.display_results(result)

    print("\n✅ Spike complete! Agent Skills approach validated.")


if __name__ == "__main__":
    main()
