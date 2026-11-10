# The viewer

The viewer shows a tickwave recording. Each channel has a row, all on one time axis. A digital
channel is drawn from its edges, and an analog channel from its samples. The same view opens on
[the Open page](../open.html) and wherever a page cites a recording, as [the demo](../demo.html)
does.

## Opening a recording

Give the Open page a recording's address, and its sidecar's if it has one. The page's address then
names both, as `?src=` and `?annotations=`, so a link to it opens the same recording. A recording on
another site opens only if that site lets other sites read its files: its server has to answer with
an `Access-Control-Allow-Origin` header (CORS).

Or open files from this computer: a `.tickwave` recording, with its sidecar's `.toml` and `.jsonl`
files if it has them. They stay in your browser, and nothing is uploaded. A link can't share them.

Under the view, the recording's details give its title, length and channels, the time one tick
stands for, its size, what it was converted from, and its content id.

## Moving and zooming

Hold Ctrl (Cmd on a Mac) and turn the wheel to zoom about the pointer, or pinch on a trackpad or a
touch screen. The plain wheel scrolls the page. Drag, swipe sideways or hold Shift with the wheel to
move along the capture.

With the view focused, the arrow keys move it, and plus and minus zoom. Home and the Fit button
show the whole capture. Escape clears the cursors and the selection.

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
