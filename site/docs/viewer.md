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
[the converter's doc](converter.html#converting-in-a-browser) says how. There, another capture or
recording can join the one shown, to merge them on one time axis, each recording lined up with the
others by hand. A reading in a merged recording keeps to the tick of the recording its channel came
from, and a span between two recordings' channels to the coarser of their ticks.

A `.zip` of these files opens as the files would. The folders inside it don't matter, but two files
with one name do: the page refuses those, since either could be the one meant. With several
recordings in it, the page lists them to open one at a time, each with the sidecar named after it.
The page reads files a zip stores as they are or deflated, as common zip tools write them.

Under the view, the recording's details give its title, length and channels, the time one tick
stands for, its size, what it was converted from, and its content id.

A recording's sidecar can store how a view of it starts: which channels it shows, in what order,
the span it opens at, and the trace's colour. The view starts that way wherever the page says
nothing else. Bundled into the recording, the sidecar goes with it, so a recording downloaded from
a page opens here as the page showed it.

A view on a page may offer Download beside Open, and Annotations where it has a sidecar. Download
saves the recording as the page serves it, its bundled sidecar with it. Annotations opens a list
of the sidecar's files: the hand-written file, then each file it includes. Each link saves one
file under its own name.

A recording can hold a channel this viewer can't read, such as one a newer tool wrote in a way
this version doesn't know. Its row keeps its place and says it is unavailable, and nothing of it
is drawn. Annotations that name it are left out, and a note under the view says which.

## Sharing a capture on GitHub

A capture attached to a GitHub issue or discussion opens in any browser, a phone's included, with
nothing to install. GitHub takes attachments of up to 25 MB, in a list of formats that leaves out
`.tickwave`. So zip the recording first, with its sidecar's files if it has them. A capture to
convert can go in the zip instead: a Saleae export's folder, or a `.sr` or `.vcd` file.

GitHub doesn't let other sites read its attachments, so the Open page can't fetch one. Give the
attachment's address to the page as a recording's address, and the page shows two steps instead:
download the zip, then choose it. The page's address is then the link to post beside the
attachment. It is [the Open page](../open.html) with `?src=` and the attachment's address, such as
`open.html?src=https://github.com/user-attachments/files/123/capture.zip`.

A reader who follows the link downloads the zip and chooses it on the page; on a phone, from its
downloads. If the browser unzips the download itself, choose the `.tickwave` file inside. The page
takes only GitHub's own attachment addresses this way. An attachment in a private repository needs
a GitHub sign-in to download, so share captures in a public one.

## Moving and zooming

Hold Ctrl (Cmd on a Mac) and turn the wheel to zoom about the pointer, or pinch on a trackpad or a
touch screen. The plain wheel scrolls the page. Drag, swipe sideways or hold Shift with the wheel to
move along the capture. A zoom step eases into place over about 150 ms. Where the system is set to
reduce motion, it jumps there instead, and so it does where the view takes longer to draw than a
frame at 60 Hz.

With the view focused, the left and right arrow keys move it, an eighth of the span shown at a
time, or half with Shift. Each move glides into place as a zoom step does, and a key held down
glides on. The up and down arrows, or plus and minus, zoom in and out. Home and the Fit button
show the whole capture, and every analog channel's whole range. Escape clears the cursors and the
selection. A key held with Alt, Ctrl or Cmd is left to the browser, so Alt with an arrow still goes
back or forward.

## Reading a channel

Point at a row to read it there. An analog channel gives the sample's value, or the smallest and
largest value where one pixel covers several samples.

On a digital channel, two spans mark the pulse under the pointer: its width, between its edges, and
its period, to the next edge the same way. A label gives the pulse's duty, its frequency and
1/width. That is the width as a rate: one UART bit's gives the baud rate. Once the row is tall
enough, the width and the period sit on their spans; until then the label gives them too.

The label sits on the edge of the rows away from the row you point at, so it never covers the wave
you are reading.

Where one pixel holds several edges, the label gives how many there are in the time the pixel
covers, and how much of that time the channel was high, then asks you to zoom in. Zoomed far out,
the view reads these from the recording's overview. It keeps each stretch's edge count to within
about 19 %, and its time high to the nearest fourteenth. There the label gives
the range those allow, such as "1,024 to 1,217 edges" and "high 32.1 to 39.3 %". A stretch with
edges and almost no time high reads "high under 3.6 %": the edges show the channel did go high.

A long pulse can end far past what the view has read, as on a quiet line seen close up. The label
then says it is measuring. Keep the pointer within the pulse and it goes on; once measured, the
pulse reads at once anywhere along it.

Where the instrument lost data, the row is shaded: where a digital channel's level was not known,
and where an analog channel lost samples, between two runs of samples, before its first or after
its last. The trace breaks there. Where the instrument only paused, the trace breaks and nothing
is shaded.

## Where edges crowd

Zoomed out, a digital channel's edges come closer together than a pixel. There the row shades the
band between its two levels instead of drawing each edge. How dark the shade is says how fast the
channel switches, in edges a second. The scale is the same at every zoom and in every recording:
faint at one edge a second or fewer, 60 % of the line's ink at 100 million and above, and darker
by the same step for every tenfold rise between. Two captures of the same activity look alike side
by side. Even the busiest band stays lighter than the line, so a crowded stretch reads as a fill.

Each level's line shades with the band, by the share of the time the channel spent at that level
over the few pixels around it. The level it spent most time at takes the band's shade; the other
takes a part of it, in proportion. An even clock's two lines both take the band's shade; a line
high three quarters of the time has its high line at the band's shade and its low line at a third
of it. Wherever the stretch holds an edge, the fainter line shows at least a fifth of the band's
shade, so a rare excursion stays visible. Outside a busy stretch a held level's line is solid, so
it steps from solid to the band's shade where a stretch begins. A short pause between two busy
stretches keeps its gap, and its line takes the band's shade.

An edge that stands apart is a line across the band. So is a short pulse or burst that stands
apart: edges too close together to show apart, spanning too little to fill a stretch, draw as one
line at their middle, whatever pixel they fall in. A flag raised for a few microseconds in an
hour reads as a solid line at every zoom, and so does each of several flags raised close
together. Where other edges come near each other without crowding into one pixel, each line fades
towards the shade a stretch of them would take, so neighbouring edges and a crowded stretch read
alike.

Zoomed in on a burst far busier than the rest of a long capture, the row goes on shading it until
the view is narrow enough to hold its edges.

## Row heights

Drag a row's lower edge in the names column to make the row taller or shorter. With that edge
focused, the up and down arrows do it too, with Shift for bigger steps. Home and End give the row
its least and greatest height, and Enter or a double click its default.

## An analog channel's scale

An analog row's scale, the values at its left, zooms and moves on its own. Hold Ctrl (Cmd on a Mac)
and turn the wheel over the scale to zoom its values about the pointer, or pinch it with two fingers
on a touch screen. Drag the scale up or down to move its values. A double click fits it to the values
in view. It zooms in no further than four steps of the instrument's resolution.

The view waits for every analog channel's overall range before it draws, so the scales and the
channel names beside them are in place from the first frame. A recording gives the range with the
first data the view loads. In a merge preview at the fastest recording's rate, a channel whose ticks
don't fall on whole ticks of that rate has no range at first: its row shows the channel's name
alone until the view has shown all of the channel, and then its scale.

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
left out. A span with more than 2,097,152 different periods, far more than a clock has, gets no
median, and its running clock is the mean of every period, the pauses with them. The readout says
so.

## Annotations

A recording can come with annotations, from its sidecar: named spans and points, shown above the
channels. Click an annotation's label to show its span.

The menu beside the zoom buttons steps through one kind of annotation, such as every WAIT a target
answered. It starts at the kind of the annotation the view opened at, or else the kind of the
sidecar's first one.

An annotation can paint its channels in a role, such as who drives a line: the probe, the target,
or neither. The line takes the role's colour, weight and dash over the annotation's span. The
sidecar declares its roles, each with a label and a look of its own, which a page can restyle. A
key beside the zoom buttons lists every role, as its lines are drawn.

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

Paint offers the roles the sidecar declares. A paint must name one of them: declare a new role in
the sidecar's file first.

A change that would break one of the sidecar's checks is refused, and the form says why. The editor
changes only what you change: the file keeps its comments, its order and its layout.

On this site, Download the sidecar saves the file to your downloads. When
[`tickwave open`](converter.html#opening-a-recording) shows the recording, Save the sidecar writes
it back beside the recording instead.
