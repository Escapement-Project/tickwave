# The viewer

The viewer shows a tickwave recording. Each channel has a row, all on one time axis. A digital
channel is drawn from its edges, and an analog channel from its samples. The same view opens on
[the Open page](../open.html) and wherever a page cites a recording, as
[tickwave's main page](../index.html#page) does.

## Opening a recording

Give the Open page a recording's address, and its sidecar's if it has one. The page's address then
names both, as `?src=` and `?annotations=`, so a link to it opens the same recording. With
`?annotation=` as well, it opens at that annotation. `?window=` gives a span to show, as two tick
counts, such as `?window=377113809+377124212`. A zoomed-in figure links to the span it shows this
way. A recording on
another site opens only if that site lets other sites read its files: its server has to answer with
an `Access-Control-Allow-Origin` header (CORS).

Or open files from this computer: a `.tickwave` recording, with its sidecar's `.toml` and `.jsonl`
files if it has them. They stay in your browser, and nothing is uploaded. A link can't share them.
A capture converts on the Open page first, where its channels can be moved and trimmed:
[the converter's doc](converter.html#converting-in-a-browser) says how.

Under the view, the recording's details give its title, length and channels, the time one tick
stands for, its size, what it was converted from, and its content id.

## Moving and zooming

Hold Ctrl (Cmd on a Mac) and turn the wheel to zoom about the pointer, or pinch on a trackpad or a
touch screen. The plain wheel scrolls the page. Drag, swipe sideways or hold Shift with the wheel to
move along the capture.

With the view focused, the left and right arrow keys move it. The up and down arrows, or plus and
minus, zoom in and out. Home and the Fit button show the whole capture, and every analog channel's
whole range. Escape clears the cursors and the selection.

## Reading a channel

Point at a row to read it there. An analog channel gives the sample's value, or the smallest and
largest value where one pixel covers several samples.

On a digital channel, two spans mark the pulse under the pointer: its width, between its edges, and
its period, to the next edge the same way. A label gives the pulse's duty, its frequency and
1/width. That is the width as a rate: one UART bit's gives the baud rate. Once the row is tall
enough, the width and the period sit on their spans; until then the label gives them too.

The label sits on the edge of the rows away from the row you point at, so it never covers the wave
you are reading. Where one pixel holds several edges, it asks you to zoom in.

A long pulse can end far past what the view has read, as on a quiet line seen close up. The label
then says it is measuring. Keep the pointer within the pulse and it goes on; once measured, the
pulse reads at once anywhere along it.

## Row heights

Drag a row's lower edge in the names column to make the row taller or shorter. With that edge
focused, the up and down arrows do it too, with Shift for bigger steps. Home and End give the row
its least and greatest height, and Enter or a double click its default.

## An analog channel's scale

An analog row's scale, the values at its left, zooms and moves on its own. Hold Ctrl (Cmd on a Mac)
and turn the wheel over the scale to zoom its values about the pointer, or pinch it with two fingers
on a touch screen. Drag the scale up or down to move its values. A double click fits it to the values
in view. It zooms in no further than four steps of the instrument's resolution.

Once the values are zoomed in, a drag over the row's traces with a mouse or a pen moves them up and
down as well as along the capture. On a touch screen, a finger moving up or down there scrolls the
page, so move the values with a finger on the scale.

With the scale focused, the up and down arrows move it, with Shift for bigger steps, and plus and
minus zoom it. Enter fits it to the values in view, and Home shows its whole range.

## Cursors and spans

A click places cursor A, and the next click cursor B. With the view focused, Enter places them at
the middle of the view. A cursor snaps to an edge within a few pixels on the row clicked.

The readout under the view gives both cursors' times and the interval between them. It gives each
analog channel's value at each cursor, and each digital channel's pulse at cursor A.

Drag along the time axis to select a span. The readout gives its length and counts each channel's
edges in it. For one channel it measures the clock: how many periods there are, their least,
median and greatest, and the running clock. That is the clock's rate and period with the pauses
left out.

## Annotations

A recording can come with annotations, from its sidecar: named spans and points, shown above the
channels. Click an annotation's label to show its span.

The menu beside the zoom buttons steps through one kind of annotation, such as every WAIT a target
answered. It starts at the kind of the annotation the view opened at, or else the kind of the
sidecar's first one.

## Editing annotations

On the Open page, Edit annotations opens the sidecar's hand-written file for editing. A recording
without a sidecar starts one.

To add an annotation, drag along the time axis to select a span, or click to place cursor A, and
press New annotation. It covers the selection, or it is a point at cursor A. Give it a label, and
its id follows the label until you type one of your own. A page cites it by its id.

Choose an annotation from the list under the form, or click its label in the view, to change it or
delete it. The arrows beside it move it up or down the list. Order matters: the view steps through
the kind of the first annotation.

A decoder writes its own annotations, and rewrites them each time it runs, so they can be read here
but not changed. Name it adds a hand-written annotation that refers to one, for a page to cite by a
name of your own. It takes the decoder's span and channels, and any field you leave empty.

A change that would break one of the sidecar's checks is refused, and the form says why. The editor
changes only what you change: the file keeps its comments, its order and its layout.

On this site, Download the sidecar saves the file to your downloads. When
[`tickwave open`](converter.html#opening-a-recording) shows the recording, Save the sidecar writes
it back beside the recording instead.
