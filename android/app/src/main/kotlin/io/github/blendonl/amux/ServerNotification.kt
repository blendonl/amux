package io.github.blendonl.amux

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.graphics.drawable.Icon
import android.os.Build

class ServerNotification(private val context: Context) {
    private val manager = context.getSystemService(NotificationManager::class.java)

    init {
        val channel = NotificationChannel(
            CHANNEL_ID,
            context.getString(R.string.notification_channel),
            NotificationManager.IMPORTANCE_LOW,
        )
        channel.description = context.getString(R.string.notification_channel_description)
        channel.setShowBadge(false)
        manager.createNotificationChannel(channel)
    }

    fun build(state: ServerState, keepingAwake: Boolean): Notification {
        val text = describe(state, keepingAwake)
        val builder = Notification.Builder(context, CHANNEL_ID)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(context.getString(R.string.notification_title))
            .setContentText(text)
            .setStyle(Notification.BigTextStyle().bigText(text))
            .setContentIntent(activity())
            .setOngoing(true)
            .setShowWhen(false)
            .setCategory(Notification.CATEGORY_SERVICE)
            .addAction(action(keepAwakeLabel(keepingAwake), AmuxService.ACTION_TOGGLE_KEEP_AWAKE))
            .addAction(action(context.getString(R.string.notification_stop), AmuxService.ACTION_STOP))
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            builder.setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE)
        }
        return builder.build()
    }

    fun show(notification: Notification) = manager.notify(ID, notification)

    private fun describe(state: ServerState, keepingAwake: Boolean): String =
        when (state) {
            ServerState.Starting -> context.getString(R.string.notification_starting)
            ServerState.Ready ->
                context.getString(if (keepingAwake) R.string.notification_running_awake else R.string.notification_running)
            is ServerState.Restarting -> state.describe(context)
            is ServerState.Failed -> state.describe(context)
            ServerState.Stopping, ServerState.Stopped -> context.getString(R.string.notification_stopping)
        }

    private fun keepAwakeLabel(keepingAwake: Boolean): String =
        context.getString(if (keepingAwake) R.string.notification_allow_sleep else R.string.notification_keep_awake)

    private fun activity(): PendingIntent =
        PendingIntent.getActivity(
            context,
            0,
            Intent(context, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )

    private fun action(label: String, action: String): Notification.Action {
        val intent = PendingIntent.getService(
            context,
            action.hashCode(),
            Intent(context, AmuxService::class.java).setAction(action),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return Notification.Action.Builder(Icon.createWithResource(context, R.drawable.ic_notification), label, intent)
            .build()
    }

    companion object {
        const val ID = 1
        private const val CHANNEL_ID = "server"
    }
}
