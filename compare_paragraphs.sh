#!/bin/bash
# Compare paragraphs between outputs

ORIGINAL="/Users/boxed/Projects/iommi/docs/_build/html/Action.html"
OURS="/Users/boxed/Projects/iommi/docs/_tmp/Action.html"

echo "=== Paragraph samples from original ==="
grep -o '<p>[^<]*</p>' "$ORIGINAL" | head -15

echo ""
echo "=== Paragraph samples from ours ==="
grep -o '<p>[^<]*</p>' "$OURS" | head -15

echo ""
echo "=== Type descriptions from original ==="
grep 'Type:' "$ORIGINAL" | head -5

echo ""
echo "=== Type descriptions from ours ==="
grep 'Type:' "$OURS" | head -5

echo ""
echo "=== Looking for paragraphs with nested content (original) ==="
grep -c '<p>' "$ORIGINAL"
grep -c '<li><p>' "$ORIGINAL"

echo ""
echo "=== Looking for paragraphs with nested content (ours) ==="
grep -c '<p>' "$OURS"
grep -c '<li><p>' "$OURS"
