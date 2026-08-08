#!/usr/bin/env python3
"""Render docs/adoption-council-report.md to docs/adoption-council-report.pdf with reportlab."""
import re
from reportlab.lib.pagesizes import letter
from reportlab.lib.units import inch
from reportlab.lib import colors
from reportlab.platypus import (BaseDocTemplate, Frame, PageTemplate, Paragraph,
                                Spacer, Table, TableStyle, HRFlowable)
from reportlab.lib.styles import ParagraphStyle

SRC = "docs/adoption-council-report.md"
DST = "docs/adoption-council-report.pdf"

VOID = colors.HexColor("#060609")
CYAN = colors.HexColor("#00b8d4")
MAGENTA = colors.HexColor("#c2247f")
INK = colors.HexColor("#1a1c26")
MUTED = colors.HexColor("#566179")

styles = {
    "title": ParagraphStyle("title", fontName="Helvetica-Bold", fontSize=20, leading=25, textColor=INK, spaceAfter=4),
    "meta": ParagraphStyle("meta", fontName="Helvetica-Oblique", fontSize=9, leading=13, textColor=MUTED, spaceAfter=10),
    "h1": ParagraphStyle("h1", fontName="Helvetica-Bold", fontSize=14, leading=18, textColor=MAGENTA, spaceBefore=16, spaceAfter=6),
    "h2": ParagraphStyle("h2", fontName="Helvetica-Bold", fontSize=11.5, leading=15, textColor=CYAN, spaceBefore=10, spaceAfter=4),
    "body": ParagraphStyle("body", fontName="Helvetica", fontSize=9.5, leading=13.5, textColor=INK, spaceAfter=6),
    "bullet": ParagraphStyle("bullet", fontName="Helvetica", fontSize=9.5, leading=13.5, textColor=INK, spaceAfter=4, leftIndent=16, bulletIndent=6),
    "cell": ParagraphStyle("cell", fontName="Helvetica", fontSize=8, leading=10.5, textColor=INK),
    "cellhead": ParagraphStyle("cellhead", fontName="Helvetica-Bold", fontSize=8, leading=10.5, textColor=colors.white),
}

def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")

def inline(s):
    s = esc(s)
    s = re.sub(r"`([^`]+)`", r'<font face="Courier" size="8.5" color="#c2247f">\1</font>', s)
    s = re.sub(r"\*\*([^*]+)\*\*", r"<b>\1</b>", s)
    s = re.sub(r"(?<!\w)\*([^*\n]+)\*(?!\w)", r"<i>\1</i>", s)
    return s

def on_page(canv, doc):
    canv.saveState()
    canv.setFillColor(MUTED)
    canv.setFont("Helvetica", 7.5)
    canv.drawString(0.75 * inch, 0.45 * inch, "Tome — Open-Source Adoption Council")
    canv.drawRightString(letter[0] - 0.75 * inch, 0.45 * inch, f"page {doc.page}")
    canv.setStrokeColor(colors.HexColor("#d5d8e4"))
    canv.setLineWidth(0.5)
    canv.line(0.75 * inch, 0.6 * inch, letter[0] - 0.75 * inch, 0.6 * inch)
    canv.restoreState()

doc = BaseDocTemplate(DST, pagesize=letter,
                      leftMargin=0.75 * inch, rightMargin=0.75 * inch,
                      topMargin=0.75 * inch, bottomMargin=0.85 * inch,
                      title="Tome — Open-Source Adoption Council", author="Tome council")
doc.addPageTemplates([PageTemplate(id="p", frames=[Frame(doc.leftMargin, doc.bottomMargin,
                                                         doc.width, doc.height, id="f")],
                                   onPage=on_page)])

story = []
lines = open(SRC).read().split("\n")
i = 0

while i < len(lines):
    line = lines[i]
    if line.startswith("|") and i + 1 < len(lines) and re.match(r"^\|[\s:|-]+\|$", lines[i + 1]):
        header = [c.strip() for c in line.strip("|").split("|")]
        rows = []
        i += 2
        while i < len(lines) and lines[i].startswith("|"):
            rows.append([c.strip() for c in lines[i].strip("|").split("|")])
            i += 1
        ncol = len(header)
        if ncol == 3:   # competitive table
            widths = [1.5 * inch, 2.6 * inch, 2.9 * inch]
        else:           # roadmap table: # Pri Item Seats Effort
            widths = [0.3 * inch, 0.55 * inch, 4.2 * inch, 1.15 * inch, 0.8 * inch]
        data = [[Paragraph(inline(h), styles["cellhead"]) for h in header]]
        data += [[Paragraph(inline(c), styles["cell"]) for c in r] for r in rows]
        t = Table(data, colWidths=widths, repeatRows=1)
        t.setStyle(TableStyle([
            ("BACKGROUND", (0, 0), (-1, 0), VOID),
            ("GRID", (0, 0), (-1, -1), 0.4, colors.HexColor("#c9cddb")),
            ("ROWBACKGROUNDS", (0, 1), (-1, -1), [colors.white, colors.HexColor("#f4f5f9")]),
            ("VALIGN", (0, 0), (-1, -1), "TOP"),
            ("TOPPADDING", (0, 0), (-1, -1), 3),
            ("BOTTOMPADDING", (0, 0), (-1, -1), 3),
        ]))
        story.append(t)
        story.append(Spacer(1, 6))
        continue
    if line.startswith("# "):
        story.append(Paragraph(inline(line[2:]), styles["title"]))
        story.append(HRFlowable(width="100%", thickness=1.5, color=MAGENTA, spaceAfter=8))
    elif line.startswith("## "):
        story.append(Paragraph(inline(line[3:]), styles["h1"]))
    elif line.startswith("### "):
        story.append(Paragraph(inline(line[4:]), styles["h2"]))
    elif line.startswith("---"):
        story.append(HRFlowable(width="100%", thickness=0.7, color=colors.HexColor("#c9cddb"), spaceBefore=6, spaceAfter=6))
    elif re.match(r"^\d+\.\s", line):
        story.append(Paragraph(inline(re.sub(r"^\d+\.\s*", "", line)), styles["bullet"],
                               bulletText=line.split(".")[0] + "."))
    elif line.startswith("- "):
        story.append(Paragraph(inline(line[2:]), styles["bullet"], bulletText="•"))
    elif not line.strip():
        pass
    elif line.startswith("**Date:**") or line.startswith("**Method:**") or line.startswith("**Verdict:") or line.startswith("**Audience:"):
        story.append(Paragraph(inline(line), styles["meta"]))
    else:
        story.append(Paragraph(inline(line), styles["body"]))
    i += 1

doc.build(story)
print("wrote", DST)
