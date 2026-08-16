// Mentor-mode scoring — pure helpers shared by the renderer's comprehension
// gate overlay and its tests. No DOM, no IPC: takes the gate's questions and
// the user's answers, returns a correct/total count for the auto-scorable
// kinds only, and maps a gate result onto an understanding-score step.
export const AUTO_TYPES = ['multiple_choice', 'true_false']

// answers is an object map (idx -> chosen value) or an array; either is
// accepted. Only multiple_choice / true_false are auto-scored — short_answer
// and code are always judged by the model, never here.
export function scoreGate(questions, answers) {
  let correct = 0
  let total = 0
  ;(questions || []).forEach((q, i) => {
    if (!AUTO_TYPES.includes(q?.type)) return
    total++
    const got = Array.isArray(answers) ? answers[i] : answers?.[String(i)]
    if (got !== undefined && String(got) === String(q.answer)) correct++
  })
  return { correct, total }
}

// Each gate contributes 0..20 points, scaled by how many auto-scorable
// questions the user got right. A gate with nothing auto-scorable moves the
// needle zero; the result is clamped to the 0..100 band.
export function nextUq(prev, { correct, total }) {
  if (!total) return prev
  const step = Math.round((correct / total) * 20)
  return Math.max(0, Math.min(100, (prev || 0) + step))
}
