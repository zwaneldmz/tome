// The comprehension gate overlay. Shown when the model (in mentor/verbose
// mode) calls gate_question; submitting answers — or skipping — completes the
// pending gate via tome.mentor.answer so the paused tool loop can resume.
import { tome, el, toast } from './util.js'
import { modalShell } from './modals.js'
import { uq, setUq } from './mentor.js'
import { scoreGate, nextUq } from '../shared/mentor-model.js'

const AUTO = new Set(['multiple_choice', 'true_false'])

export function showMentorOverlay({ id, questions, test_code: testCode, summary }) {
  // A gate answered once must not be answered twice: modalShell's close()
  // always fires onClose (Escape, scrim click, or an explicit m.close()),
  // so route every path through `complete`, which is idempotent.
  let settled = false
  const complete = (answers, skip) => {
    if (settled) return
    settled = true
    tome.mentor.answer(id, answers, skip)
  }

  // Escape / scrim = skip, so the model is never left hanging on a gate the
  // user dismissed rather than answered.
  const m = modalShell('Prove you understand', () => complete(null, true))
  const box = m.body.parentElement
  const overlay = box.parentElement

  if (testCode) {
    m.body.appendChild(el('p', 'mentor-label', 'Write this failing test first'))
    const pre = el('pre', 'mentor-test')
    pre.textContent = testCode
    m.body.appendChild(pre)
  }
  if (summary) m.body.appendChild(el('p', 'ag-note', summary))

  // Answers keyed by question index (string), the shape scoreGate expects.
  const answers = {}

  ;(questions || []).forEach((q, i) => {
    const key = String(i)
    m.body.appendChild(el('p', 'mentor-q', q.prompt))

    if (q.type === 'multiple_choice' || q.type === 'true_false') {
      const group = el('div', 'mentor-opts')
      const options =
        q.type === 'true_false' ? ['True', 'False'] : q.options || []
      for (const opt of options) {
        const label = el('label', 'mentor-opt')
        const radio = document.createElement('input')
        radio.type = 'radio'
        radio.name = `mentor-q-${i}`
        radio.value = opt
        radio.addEventListener('change', () => (answers[key] = opt))
        label.append(radio, el('span', '', opt))
        group.appendChild(label)
      }
      m.body.appendChild(group)
    } else {
      // short_answer / code — the model judges these; never auto-scored.
      const ta = el('textarea', 'mentor-input')
      ta.rows = q.type === 'code' ? 5 : 3
      ta.spellcheck = false
      ta.addEventListener('input', () => (answers[key] = ta.value))
      m.body.appendChild(ta)
    }
  })

  const submit = () => {
    let missing = false
    ;(questions || []).forEach((q, i) => {
      if (!AUTO.has(q?.type)) return
      const got = answers[String(i)]
      if (got === undefined || got === null || got === '') missing = true
    })
    if (missing) return toast('answer every question or skip')
    const { correct, total } = scoreGate(questions, answers)
    setUq(nextUq(uq(), { correct, total }))
    complete(answers, false)
    m.close()
  }

  m.button('Submit', submit)

  // Pinned to the overlay (not the body) so CSS can hold it bottom-left of
  // the full-screen scrim, separate from the box's own buttons.
  const skip = el('button', 'mentor-skip', 'Skip test')
  skip.type = 'button'
  skip.addEventListener('click', () => {
    complete(null, true)
    m.close()
  })
  overlay.appendChild(skip)
}
