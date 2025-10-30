#!/usr/bin/env python3
"""
Batch Ground Truth Generator for Code Session Evaluations

Uses Anthropic's Batch API to create high-quality ground truth evaluations
at scale (50% cost savings vs streaming).

Process:
1. Fetch narratives from Qdrant v3_all_projects collection
2. Create batch evaluation requests using GRADER_PROMPT.md
3. Submit to Batch API ($0.015 per eval instead of $0.30)
4. Retrieve results after 24 hours
5. Push ground truth to new Qdrant collection: ground_truth_evals

Why Batch API?
- 50% cost: $0.75 for 50 evals vs $15 streaming
- Parallel processing: All 50 done in 24hrs
- High quality: Uses Claude Opus 4 for grading
- Reproducible: Same prompts, consistent results
"""

import json
import anthropic
import time
from pathlib import Path
from typing import Dict, List, Any, Optional
from datetime import datetime
import requests
import os
from dotenv import load_dotenv

# Load environment variables from .env
load_dotenv()


class BatchGroundTruthGenerator:
    """
    Generate ground truth evaluations using Batch API.
    Stores results in Qdrant for calibration and testing.
    """

    def __init__(self):
        """Initialize with Anthropic client and Qdrant connection."""
        # Validate API key
        api_key = os.getenv("ANTHROPIC_API_KEY")
        if not api_key:
            raise ValueError(
                "ANTHROPIC_API_KEY environment variable required. "
                "Set it in your .env file or export it in your shell."
            )

        # Initialize Anthropic client
        self.client = anthropic.Anthropic(api_key=api_key)
        self.qdrant_url = "http://localhost:6333"
        self.collection_name = "v3_all_projects"
        self.ground_truth_collection = "ground_truth_evals"

        # Load grader prompt template
        prompt_path = Path(__file__).parent / "GRADER_PROMPT.md"
        with open(prompt_path, 'r') as f:
            self.grader_prompt_template = f.read()

    def fetch_narratives_from_qdrant(
        self,
        limit: int = 100,
        filters: Optional[Dict] = None
    ) -> List[Dict]:
        """
        Fetch narratives from Qdrant for evaluation.

        Args:
            limit: Number of narratives to fetch
            filters: Optional Qdrant filters (e.g., only successful builds)

        Returns:
            List of narrative payloads with metadata
        """
        url = f"{self.qdrant_url}/collections/{self.collection_name}/points/scroll"

        payload = {
            "limit": limit,
            "with_payload": True,
            "with_vector": False
        }

        if filters:
            payload["filter"] = filters

        response = requests.post(url, json=payload)
        response.raise_for_status()

        points = response.json()["result"]["points"]
        print(f"✅ Fetched {len(points)} narratives from Qdrant")

        return points

    def create_batch_requests(
        self,
        narratives: List[Dict],
        output_file: str = "batch_ground_truth_requests.jsonl",
        model: str = "claude-haiku-4-5"
    ) -> str:
        """
        Create batch API request file from narratives.

        Each request evaluates one conversation using GRADER_PROMPT.md.

        Uses Haiku 4.5 by default for:
        - Fast processing (minutes instead of 24 hours)
        - Lower cost (~$0.001 per eval vs $0.015 for Opus)
        - Still high quality for grading tasks

        Format per line:
        {
          "custom_id": "conversation_id",
          "params": {
            "model": "claude-haiku-4.5",
            "max_tokens": 4096,
            "messages": [
              {"role": "user", "content": "<grader_prompt>"}
            ]
          }
        }
        """
        requests_data = []

        for point in narratives:
            payload = point["payload"]
            conversation_id = payload.get("conversation_id")
            narrative = payload.get("narrative", "")
            search_index = payload.get("search_index", "")
            context_cache = payload.get("context_cache", "")
            signature = payload.get("signature", {})

            # Build grader prompt
            grader_input = self._build_grader_input(
                conversation_id,
                narrative,
                search_index,
                context_cache,
                signature
            )

            # Create batch request
            batch_request = {
                "custom_id": conversation_id,
                "params": {
                    "model": model,
                    "max_tokens": 4096,
                    "messages": [
                        {
                            "role": "user",
                            "content": grader_input
                        }
                    ]
                }
            }

            requests_data.append(batch_request)

        # Write to JSONL file
        output_path = Path(__file__).parent / output_file
        with open(output_path, 'w') as f:
            for req in requests_data:
                f.write(json.dumps(req) + '\n')

        print(f"✅ Created {len(requests_data)} batch requests in {output_file}")
        return str(output_path)

    def _build_grader_input(
        self,
        conversation_id: str,
        narrative: str,
        search_index: str,
        context_cache: str,
        signature: Dict
    ) -> str:
        """
        Build grader prompt for one conversation.

        Extracts:
        - User request (from search_index)
        - Solution (from narrative)
        - Build/test results (from context_cache validation section)
        - Completion status (from signature)
        """
        # Extract user request
        request_lines = search_index.split("## User Request")
        user_request = request_lines[1].split("##")[0].strip() if len(request_lines) > 1 else "Unknown"

        # Extract validation signals from context_cache
        validation_section = context_cache.split("## Validation")[1] if "## Validation" in context_cache else ""

        # Count builds and tests
        build_success_count = validation_section.count("Build: Success")
        test_passed_count = validation_section.count("Tests: Passed")

        # Build tier1_results XML
        tier1_xml = f"""<tier1_results>
  <build_success>{build_success_count > 0}</build_success>
  <test_results>
    <passed>{test_passed_count}</passed>
    <failed>0</failed>
    <framework>unknown</framework>
  </test_results>
  <code_quality>0.0</code_quality>
  <security_issues>0</security_issues>
  <confidence>0.6</confidence>
</tier1_results>"""

        # Extract solution from narrative
        solution_section = narrative.split("## Technical Pattern")[1].split("##")[0] if "## Technical Pattern" in narrative else narrative[:500]

        # Build rubric from completion status
        completion = signature.get("completion_status", "unknown")
        rubric = f"""
- Solution must address the user's request
- Code must be functional (builds and tests pass)
- Implementation should follow best practices
- Expected completion status: {completion}
"""

        # Fill in grader prompt template
        grader_prompt = f"""You are evaluating a code generation session for ground truth labeling.

<request>{user_request}</request>

<solution>
{solution_section}
</solution>

{tier1_xml}

<rubric>
{rubric}
</rubric>

<narrative>
Full narrative for context:
{narrative[:1000]}...
</narrative>

Please evaluate this session and provide:
1. Functional correctness score (0.0-1.0)
2. Design quality score (0.0-1.0)
3. Overall grade (0.0-1.0)
4. Reasoning for your scores

Output in XML format as specified in the grader prompt.
"""

        return grader_prompt

    def submit_batch(self, requests_file: str) -> str:
        """
        Submit batch to Anthropic API.

        Returns:
            batch_id for tracking
        """
        # Read JSONL file and parse requests
        requests = []
        with open(requests_file, 'r') as f:
            for line in f:
                if line.strip():
                    requests.append(json.loads(line))

        # Submit batch with parsed requests
        batch = self.client.messages.batches.create(
            requests=requests
        )

        print(f"✅ Batch submitted: {batch.id}")
        print(f"   Status: {batch.processing_status}")
        print(f"   Request count: {batch.request_counts}")

        return batch.id

    def check_batch_status(self, batch_id: str) -> Dict:
        """
        Check batch processing status.

        Returns:
            Batch status object
        """
        batch = self.client.messages.batches.retrieve(batch_id)

        print(f"📊 Batch {batch_id}:")
        print(f"   Status: {batch.processing_status}")
        print(f"   Succeeded: {batch.request_counts.succeeded}")
        print(f"   Failed: {batch.request_counts.errored}")
        print(f"   Total: {batch.request_counts.processing}")

        return {
            "status": batch.processing_status,
            "succeeded": batch.request_counts.succeeded,
            "failed": batch.request_counts.errored,
            "total": batch.request_counts.processing
        }

    def retrieve_batch_results(
        self,
        batch_id: str,
        output_file: str = "batch_ground_truth_results.jsonl"
    ) -> str:
        """
        Retrieve completed batch results.

        Returns:
            Path to results file
        """
        # Stream results to file
        output_path = Path(__file__).parent / output_file

        with open(output_path, 'w') as f:
            for result in self.client.messages.batches.results(batch_id):
                f.write(json.dumps(result.model_dump()) + '\n')

        print(f"✅ Retrieved batch results to {output_file}")
        return str(output_path)

    def parse_batch_results(self, results_file: str) -> List[Dict]:
        """
        Parse batch results into ground truth evaluations.

        Each result contains:
        - custom_id (conversation_id)
        - result (API response with evaluation)
        """
        ground_truths = []

        with open(results_file, 'r') as f:
            for line in f:
                result = json.loads(line)

                # Extract evaluation from response
                custom_id = result.get("custom_id")
                response = result.get("result", {})

                if response.get("type") == "succeeded":
                    message = response.get("message", {})
                    content = message.get("content", [])

                    # Extract evaluation XML
                    eval_text = content[0].get("text", "") if content else ""

                    # Parse evaluation (would need proper XML parsing)
                    ground_truth = {
                        "conversation_id": custom_id,
                        "evaluation": eval_text,
                        "timestamp": datetime.utcnow().isoformat(),
                        "model": "claude-haiku-4.5",
                        "method": "batch_api"
                    }

                    ground_truths.append(ground_truth)

        print(f"✅ Parsed {len(ground_truths)} ground truth evaluations")
        return ground_truths

    def create_ground_truth_collection(self):
        """
        Create Qdrant collection for ground truth evaluations.

        Schema:
        - conversation_id: str
        - evaluation: str (XML from grader)
        - scores: {functional, design, overall}
        - timestamp: str
        - model: str
        """
        url = f"{self.qdrant_url}/collections/{self.ground_truth_collection}"

        # Create collection with same vector size as narratives
        payload = {
            "vectors": {
                "size": 384,  # FastEmbed dimensions
                "distance": "Cosine"
            }
        }

        response = requests.put(url, json=payload)
        if response.status_code == 200:
            print(f"✅ Created collection: {self.ground_truth_collection}")
        else:
            print(f"ℹ️  Collection already exists or error: {response.status_code}")

    def push_to_qdrant(self, ground_truths: List[Dict]):
        """
        Push ground truth evaluations to Qdrant.

        Note: We don't need embeddings for ground truth,
        just storing for reference and calibration.
        """
        url = f"{self.qdrant_url}/collections/{self.ground_truth_collection}/points"

        points = []
        for gt in ground_truths:
            point = {
                "id": gt["conversation_id"],
                "vector": [0.0] * 384,  # Dummy vector, not used for search
                "payload": gt
            }
            points.append(point)

        # Batch upsert
        payload = {"points": points}
        response = requests.put(url, json=payload)
        response.raise_for_status()

        print(f"✅ Pushed {len(points)} ground truths to Qdrant")


def main():
    """
    Example workflow:

    Step 1: Generate batch requests (today)
    Step 2: Submit to Batch API (today)
    Step 3: Wait 24 hours
    Step 4: Retrieve results (tomorrow)
    Step 5: Push to Qdrant (tomorrow)
    """
    generator = BatchGroundTruthGenerator()

    # Step 1: Fetch narratives
    print("Step 1: Fetching narratives from Qdrant...")
    narratives = generator.fetch_narratives_from_qdrant(limit=50)

    # Step 2: Create batch requests
    print("\nStep 2: Creating batch requests...")
    requests_file = generator.create_batch_requests(narratives)

    # Step 3: Submit batch
    print("\nStep 3: Submitting batch to Anthropic API...")
    batch_id = generator.submit_batch(requests_file)

    print(f"\n✅ Batch submitted successfully!")
    print(f"   Batch ID: {batch_id}")
    print(f"   Processing time: ~5-10 minutes (Haiku 4.5)")
    print(f"   Cost: ~${50 * 0.001:.2f} for 50 evaluations")

    # Save batch ID for retrieval
    with open("batch_ground_truth_id.txt", "w") as f:
        f.write(batch_id)

    print(f"\n💾 Batch ID saved to batch_ground_truth_id.txt")
    print(f"\n⏰ Run retrieve_results() in 10 minutes to get evaluations")


def retrieve_results():
    """
    Run this after 24 hours to retrieve batch results.
    """
    # Load batch ID
    with open("batch_ground_truth_id.txt", "r") as f:
        batch_id = f.read().strip()

    generator = BatchGroundTruthGenerator()

    # Check status
    print("Checking batch status...")
    status = generator.check_batch_status(batch_id)

    if status["status"] != "ended":
        print(f"⏳ Batch still processing. Status: {status['status']}")
        return

    # Retrieve results
    print("\nRetrieving results...")
    results_file = generator.retrieve_batch_results(batch_id)

    # Parse results
    print("\nParsing evaluations...")
    ground_truths = generator.parse_batch_results(results_file)

    # Create collection
    print("\nCreating Qdrant collection...")
    generator.create_ground_truth_collection()

    # Push to Qdrant
    print("\nPushing to Qdrant...")
    generator.push_to_qdrant(ground_truths)

    print(f"\n✅ Ground truth generation complete!")
    print(f"   {len(ground_truths)} evaluations stored in Qdrant")
    print(f"   Collection: {generator.ground_truth_collection}")


if __name__ == "__main__":
    import sys

    if len(sys.argv) > 1 and sys.argv[1] == "retrieve":
        retrieve_results()
    else:
        main()
