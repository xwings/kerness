#!/usr/bin/env python3
"""Session + gameplan bidding workflow with structured output."""

from __future__ import annotations

import os
import sys

from pydantic import BaseModel, Field

import kerness


class BidDecision(BaseModel):
    vendor_name: str
    project_name: str
    pass_gate: bool = Field(description="Whether this vendor should enter final round")
    score: int = Field(description="0-100 overall bid score")
    recommended_bid_cny: int = Field(description="Recommended bid price in CNY")
    key_risks: list[str]
    rationale: str


def build_structured_provider(api_key: str) -> kerness.OpenAIProvider:
    return kerness.OpenAIProvider(
        api_key=api_key,
        output_type=BidDecision,
        strict_json_schema=True,
        output_schema_name="bid_decision",
    )


def run_bid_review_session(
    *,
    api_key: str,
    model: str,
    vendor_name: str,
    project_name: str,
    requirements: str,
    vendor_proposal: str,
) -> kerness.SessionResult:
    """Run a multi-agent review session using the built-in discussion gameplan."""
    session = kerness.Session(
        gameplan="discussion",
        topic=(
            f"Project: {project_name}\n"
            f"Vendor: {vendor_name}\n\n"
            f"Requirements:\n{requirements}\n\n"
            f"Vendor proposal:\n{vendor_proposal}"
        ),
        provider=kerness.OpenAIProvider(api_key=api_key),
        channel=kerness.ConsoleChannel(),
        max_turns=12,
    )

    session.add_agent(
        name="Procurement Lead",
        model=model,
        persona="Procurement manager focused on commercial terms and delivery risk.",
    )
    session.add_agent(
        name="Technical Architect",
        model=model,
        persona="Senior architect focused on feasibility, integration, and maintainability.",
    )
    session.add_agent(
        name="Finance Analyst",
        model=model,
        persona="Finance analyst focused on cost realism and budget compliance.",
    )
    session.add_agent(
        name="Bid Committee Chair",
        model=model,
        role="orchestrator",
        persona="Neutral facilitator who drives evidence-based conclusions.",
    )

    session.add_skill("summarize")
    session.add_skill("fact_check")

    return session.run()


def build_structured_decision(
    *,
    api_key: str,
    model: str,
    vendor_name: str,
    project_name: str,
    requirements: str,
    vendor_proposal: str,
) -> BidDecision:
    """Convert session findings into a strict structured decision."""
    session_result = run_bid_review_session(
        api_key=api_key,
        model=model,
        vendor_name=vendor_name,
        project_name=project_name,
        requirements=requirements,
        vendor_proposal=vendor_proposal,
    )

    provider = build_structured_provider(api_key)
    messages = [
        {
            "role": "system",
            "content": (
                "You are a procurement evaluation assistant. "
                "Use the committee session summary to produce the final decision. "
                "Return strict JSON that matches the schema."
            ),
        },
        {
            "role": "user",
            "content": (
                f"Project: {project_name}\n"
                f"Vendor: {vendor_name}\n\n"
                f"Requirements:\n{requirements}\n\n"
                f"Vendor proposal:\n{vendor_proposal}\n\n"
                f"Session summary:\n{session_result.summary}\n\n"
                "Give: pass_gate, score, recommended_bid_cny, key_risks, rationale."
            ),
        },
    ]

    response = provider.chat(model, messages)
    result = response.structured

    if result is None:
        raise RuntimeError("No structured output returned.")

    return result


def main() -> None:
    api_key = os.environ.get("OPENAI_API_KEY", "")
    if not api_key:
        print("Set OPENAI_API_KEY environment variable first.")
        sys.exit(1)

    result = build_structured_decision(
        api_key=api_key,
        model="gpt-4o",
        vendor_name="Northstar Software",
        project_name="Customer Support Ticket Routing System",
        requirements=(
            "1) MVP must go live in 2 months; 2) Must support on-prem deployment; "
            "3) Must integrate with the existing CRM; "
            "4) Annual budget must not exceed CNY 450,000; "
            "5) Critical incidents require response within 4 hours."
        ),
        vendor_proposal=(
            "Quoted CNY 520,000, promises MVP in 6 weeks, and supports on-prem deployment. "
            "CRM integration needs about 3 extra weeks of custom work. "
            "SLA response time is 8 hours for critical incidents. "
            "Vendor has delivered 2 similar projects before."
        ),
    )

    print("Structured result:")
    print(result.model_dump_json(indent=2))


if __name__ == "__main__":
    main()
