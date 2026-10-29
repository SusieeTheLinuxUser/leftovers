import QtQuick
import QtQuick.Layouts
import org.kde.plasma.plasmoid
import org.kde.plasma.core as PlasmaCore
import org.kde.plasma.components as PlasmaComponents
import org.kde.plasma.plasma5support as P5Support
import org.kde.kirigami as Kirigami

PlasmoidItem {
    id: root
    property var providers: []
    property real now: Date.now()
    readonly property string script: Qt.resolvedUrl("../code/limits.py").toString().replace("file://", "")
    readonly property var tints: ({ Claude: "#d97757", Codex: "#ffffff", Cursor: "#a78bfa", Kimi: "#3b82f6" })
    // QML hands JSON arrays back as list types without flatMap, hence the plain loop.
    readonly property var all: {
        const out = []
        for (const p of providers)
            for (const l of p.limits)
                out.push(Object.assign({ provider: p.name, account: p.account }, l))
        return out
    }
    // The panel shows whichever limit is closest to running out.
    readonly property var lowest: all.reduce((a, b) => (!a || b.used > a.used ? b : a), null)
    readonly property real lowestLeft: lowest ? Math.max(0, 100 - lowest.used) : 0
    // One section per provider; extra accounts are flipped through with the arrows.
    readonly property var groups: {
        const out = []
        for (const p of providers) {
            const g = out.find(g => g.name === p.name)
            if (g) g.accounts.push(p)
            else out.push({ name: p.name, accounts: [p] })
        }
        return out
    }
    property var picks: ({})  // provider name -> account index, kept on root so refreshes don't reset it

    function tint(name) { return tints[name] || "#a3a3a3" }

    Plasmoid.backgroundHints: PlasmaCore.Types.NoBackground  // on the desktop the black card is the background
    toolTipMainText: "Leftovers"
    toolTipSubText: all.map(l => `${l.provider}${l.account ? ` (${l.account})` : ""} ${l.name}: ${Math.round(100 - l.used)}% left`).join("\n")

    P5Support.DataSource {
        id: exec
        engine: "executable"
        onNewData: (source, data) => {
            disconnectSource(source)
            try {
                root.providers = JSON.parse(data.stdout).providers
            } catch (e) {
                console.warn("leftovers:", data.stderr)
            }
        }
    }

    // Claude's usage endpoint returns 429 when polled much faster than this.
    Timer {
        interval: 5 * 60 * 1000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: exec.connectSource(`python3 '${root.script}'`)
    }
    Timer {
        interval: 30 * 1000
        running: true
        repeat: true
        onTriggered: root.now = Date.now()
    }

    compactRepresentation: MouseArea {
        Layout.minimumWidth: compactLabel.implicitWidth + Kirigami.Units.smallSpacing * 2
        onClicked: root.expanded = !root.expanded
        PlasmaComponents.Label {
            id: compactLabel
            anchors.centerIn: parent
            text: root.lowest ? `${Math.round(root.lowestLeft)}%` : "…"
            color: root.lowest ? root.tint(root.lowest.provider) : Kirigami.Theme.textColor
            font.weight: Font.DemiBold
        }
        Rectangle {
            visible: !!root.lowest
            anchors { top: compactLabel.bottom; left: compactLabel.left; right: compactLabel.right }
            height: 2
            radius: 1
            color: Qt.rgba(1, 1, 1, 0.12)
            Rectangle {
                width: parent.width * root.lowestLeft / 100
                height: parent.height
                radius: parent.radius
                color: compactLabel.color
            }
        }
    }

    fullRepresentation: Rectangle {
        Layout.preferredWidth: Kirigami.Units.gridUnit * 20
        Layout.preferredHeight: content.implicitHeight + content.anchors.margins * 2
        color: "#000000"
        radius: 10

        ColumnLayout {
            id: content
            anchors { fill: parent; margins: Kirigami.Units.gridUnit }
            spacing: Kirigami.Units.largeSpacing

            Repeater {
                model: root.groups
                delegate: ColumnLayout {
                    id: provider
                    required property var modelData
                    required property int index
                    readonly property var group: modelData
                    readonly property int count: group.accounts.length
                    readonly property int pick: (root.picks[group.name] || 0) % count
                    readonly property var account: group.accounts[pick]
                    readonly property color tint: root.tint(group.name)
                    function flip(step) {
                        const p = Object.assign({}, root.picks)
                        p[group.name] = (pick + step + count) % count
                        root.picks = p
                    }
                    Layout.fillWidth: true
                    spacing: Kirigami.Units.smallSpacing * 1.5

                    Rectangle {
                        visible: provider.index > 0
                        Layout.fillWidth: true
                        Layout.bottomMargin: Kirigami.Units.smallSpacing
                        implicitHeight: 1
                        color: "#141414"
                    }
                    RowLayout {
                        spacing: Kirigami.Units.smallSpacing * 1.5
                        Rectangle {
                            implicitWidth: 6
                            implicitHeight: 6
                            radius: 3
                            color: provider.tint
                        }
                        Text {
                            text: provider.group.name
                            color: provider.tint
                            font.weight: Font.DemiBold
                        }
                        Repeater {
                            model: provider.count > 1 ? [-1, 1] : []
                            delegate: Text {
                                required property int modelData
                                text: modelData < 0 ? "‹" : "›"
                                color: arrow.containsMouse ? provider.tint : "#5c5c5c"
                                font.weight: Font.DemiBold
                                font.pointSize: Kirigami.Theme.defaultFont.pointSize * 1.3
                                leftPadding: Kirigami.Units.smallSpacing
                                rightPadding: Kirigami.Units.smallSpacing
                                MouseArea {
                                    id: arrow
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: provider.flip(parent.modelData)
                                }
                            }
                        }
                        Text {
                            text: (provider.count > 1 ? `${provider.pick + 1}/${provider.count} · ` : "") + provider.account.account
                            color: "#5c5c5c"
                            font: Kirigami.Theme.smallFont
                            elide: Text.ElideMiddle
                            horizontalAlignment: Text.AlignRight
                            Layout.fillWidth: true
                        }
                    }
                    Text {
                        visible: !!provider.account.error
                        text: (provider.account.limits.length ? "last known numbers · " : "") + provider.account.error
                        color: "#5c5c5c"
                        font: Kirigami.Theme.smallFont
                        wrapMode: Text.Wrap
                        Layout.fillWidth: true
                    }
                    Repeater {
                        model: provider.account.limits
                        delegate: Bar {
                            required property var modelData
                            Layout.fillWidth: true
                            label: modelData.name
                            used: modelData.used
                            resets: modelData.resets
                            tint: provider.tint
                            now: root.now
                        }
                    }
                }
            }
            Text {
                visible: root.providers.length === 0
                text: "Log in to claude, codex, cursor-agent or kimi in a terminal and they'll show up here."
                color: "#a3a3a3"
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }
        }
    }
}
