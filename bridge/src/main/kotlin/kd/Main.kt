package kd

import kotlinx.coroutines.*
import okhttp3.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.ResponseBody.Companion.toResponseBody
import okhttp3.RequestBody.Companion.toRequestBody
import org.json.JSONArray
import org.json.JSONObject
import org.koitharu.kotatsu.parsers.MangaLoaderContext
import org.koitharu.kotatsu.parsers.MangaParser
import org.koitharu.kotatsu.parsers.ParsersBundle
import org.koitharu.kotatsu.parsers.bitmap.Bitmap
import org.koitharu.kotatsu.parsers.bitmap.Rect
import org.koitharu.kotatsu.parsers.config.ConfigKey
import org.koitharu.kotatsu.parsers.config.MangaSourceConfig
import org.koitharu.kotatsu.parsers.core.ParsersBundleImpl
import org.koitharu.kotatsu.parsers.model.*
import org.koitharu.kotatsu.parsers.util.await
import java.awt.image.BufferedImage
import java.io.*
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import javax.imageio.ImageIO

/*
 * Line-delimited JSON RPC over stdin/stdout.
 *   request : {"id":1,"m":"list","p":{...}}
 *   response: {"id":1,"r":<result>}  or  {"id":1,"e":"message"}
 * Requests run concurrently; {"m":"cancel","p":{"id":N}} cancels one in flight.
 */

private const val UA = "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0"

// ---------------------------------------------------------------- cookies

class FileCookieJar(private val file: File) : CookieJar {
	private val store = ConcurrentHashMap<String, Cookie>()

	init {
		runCatching {
			if (file.exists()) file.forEachLine { line ->
				val i = line.indexOf('\t')
				if (i > 0) {
					val url = line.substring(0, i).toHttpUrl()
					Cookie.parse(url, line.substring(i + 1))?.let { put(it) }
				}
			}
		}
	}

	private fun key(c: Cookie) = "${c.domain}|${c.path}|${c.name}"
	private fun put(c: Cookie) {
		if (c.expiresAt < System.currentTimeMillis()) store.remove(key(c)) else store[key(c)] = c
	}

	@Volatile private var dirty = false

	override fun saveFromResponse(url: HttpUrl, cookies: List<Cookie>) {
		cookies.forEach(::put); dirty = true
	}

	override fun loadForRequest(url: HttpUrl): List<Cookie> {
		val now = System.currentTimeMillis()
		return store.values.filter { it.expiresAt > now && it.matches(url) }
	}

	fun flush() {
		if (!dirty) return
		dirty = false
		runCatching {
			file.parentFile?.mkdirs()
			val tmp = File(file.path + ".tmp")
			tmp.bufferedWriter().use { w ->
				store.values.filter { it.persistent }.forEach { c ->
					val host = c.domain.trimStart('.')
					w.write("https://$host${c.path}\t$c\n")
				}
			}
			tmp.renameTo(file)
		}
	}
}

// ---------------------------------------------------------------- bitmaps (descrambling)

class AwtBitmap(val image: BufferedImage) : Bitmap {
	override val width get() = image.width
	override val height get() = image.height
	override fun drawBitmap(sourceBitmap: Bitmap, src: Rect, dst: Rect) {
		val g = image.createGraphics()
		val sub = (sourceBitmap as AwtBitmap).image.getSubimage(src.left, src.top, src.width, src.height)
		g.drawImage(sub, dst.left, dst.top, dst.width, dst.height, null)
		g.dispose()
	}
}

// ---------------------------------------------------------------- context

class Ctx(dataDir: File) : MangaLoaderContext() {
	override val cookieJar = FileCookieJar(File(dataDir, "cookies.txt"))
	override val parsers: ParsersBundle get() = ParsersBundleImpl
	private val instances = ConcurrentHashMap<String, MangaParser>()
	private val domainOverrides = ConcurrentHashMap<String, String>()

	override val httpClient: OkHttpClient = OkHttpClient.Builder()
		.cookieJar(cookieJar)
		.addInterceptor { chain ->
			// Route through the source's own interceptor first (its headers/UA, descrambling, ...)
			val src = chain.request().tag(MangaSource::class.java)
			val p = (src as? ParserMangaSource)?.let { parser(it.name) }
			p?.intercept(chain) ?: chain.proceed(chain.request())
		}
		.addInterceptor { chain ->
			val req = chain.request()
			if (req.header("User-Agent") != null) chain.proceed(req)
			else chain.proceed(req.newBuilder().header("User-Agent", UA).build())
		}
		.connectionPool(ConnectionPool(16, 2, TimeUnit.MINUTES))
		.dispatcher(Dispatcher().apply { maxRequests = 64; maxRequestsPerHost = 6 })
		.connectTimeout(15, TimeUnit.SECONDS)
		.readTimeout(45, TimeUnit.SECONDS)
		.build()

	fun parser(name: String): MangaParser = instances.getOrPut(name) {
		val src = parsers.findSource(name) ?: throw IllegalArgumentException("Unknown source $name")
		newParserInstance(src)
	}

	fun setDomain(source: String, domain: String?) {
		if (domain.isNullOrBlank()) domainOverrides.remove(source) else domainOverrides[source] = domain
	}

	@Deprecated("Provide a base url")
	override suspend fun evaluateJs(script: String): String? = null
	override suspend fun evaluateJs(baseUrl: String, script: String, timeout: Long): String? = null

	override fun getConfig(source: MangaSource): MangaSourceConfig = object : MangaSourceConfig {
		@Suppress("UNCHECKED_CAST")
		override fun <T> get(key: ConfigKey<T>): T {
			if (key is ConfigKey.Domain) domainOverrides[source.name]?.let { return it as T }
			return key.defaultValue
		}
	}

	override fun getDefaultUserAgent(): String = UA

	override fun redrawImageResponse(response: Response, redraw: (image: Bitmap) -> Bitmap): Response {
		val src = response.body.byteStream().use(ImageIO::read) ?: error("Cannot decode image")
		val res = redraw(AwtBitmap(src)) as AwtBitmap
		val out = ByteArrayOutputStream()
		ImageIO.write(res.image, "png", out)
		return response.newBuilder().body(out.toByteArray().toResponseBody("image/png".toMediaTypeOrNull())).build()
	}

	override fun createBitmap(width: Int, height: Int): Bitmap =
		AwtBitmap(BufferedImage(width, height, BufferedImage.TYPE_INT_RGB))
}

// ---------------------------------------------------------------- JSON mapping

fun JSONObject.str(k: String): String? = if (has(k) && !isNull(k)) getString(k) else null

fun srcOf(ctx: Ctx, name: String): MangaSource = ctx.parsers.findSource(name) ?: error("Unknown source $name")

fun tagJson(t: MangaTag) = JSONObject().put("key", t.key).put("title", t.title)

fun chapterJson(c: MangaChapter) = JSONObject()
	.put("id", c.id).put("title", c.title).put("number", c.number.toDouble()).put("volume", c.volume)
	.put("url", c.url).put("scanlator", c.scanlator).put("date", c.uploadDate).put("branch", c.branch)

fun mangaJson(m: Manga): JSONObject = JSONObject()
	.put("id", m.id).put("title", m.title).put("alt", JSONArray(m.altTitles))
	.put("url", m.url).put("publicUrl", m.publicUrl).put("rating", m.rating.toDouble())
	.put("adult", m.contentRating == ContentRating.ADULT)
	.put("cover", m.coverUrl).put("largeCover", m.largeCoverUrl)
	.put("tags", JSONArray(m.tags.map(::tagJson))).put("state", m.state?.name)
	.put("authors", JSONArray(m.authors)).put("description", m.description)
	.put("source", m.source.name)
	.also { o -> m.chapters?.let { o.put("chapters", JSONArray(it.map(::chapterJson))) } }

fun mangaFrom(ctx: Ctx, o: JSONObject): Manga {
	val src = srcOf(ctx, o.getString("source"))
	return Manga(
		id = o.getLong("id"),
		title = o.getString("title"),
		altTitles = o.optJSONArray("alt")?.let { a -> (0 until a.length()).map { a.getString(it) }.toSet() } ?: emptySet(),
		url = o.getString("url"),
		publicUrl = o.optString("publicUrl", ""),
		rating = o.optDouble("rating", -1.0).toFloat(),
		contentRating = if (o.optBoolean("adult")) ContentRating.ADULT else null,
		coverUrl = o.str("cover"),
		tags = o.optJSONArray("tags")?.let { a ->
			(0 until a.length()).map { a.getJSONObject(it) }.map { MangaTag(it.getString("title"), it.getString("key"), src) }.toSet()
		} ?: emptySet(),
		state = o.str("state")?.let { runCatching { MangaState.valueOf(it) }.getOrNull() },
		authors = o.optJSONArray("authors")?.let { a -> (0 until a.length()).map { a.getString(it) }.toSet() } ?: emptySet(),
		largeCoverUrl = o.str("largeCover"),
		description = o.str("description"),
		source = src,
	)
}

fun chapterFrom(ctx: Ctx, source: String, o: JSONObject) = MangaChapter(
	id = o.getLong("id"), title = o.str("title"), number = o.optDouble("number", 0.0).toFloat(),
	volume = o.optInt("volume", 0), url = o.getString("url"), scanlator = o.str("scanlator"),
	uploadDate = o.optLong("date", 0), branch = o.str("branch"), source = srcOf(ctx, source),
)

// ---------------------------------------------------------------- handlers

class Bridge(private val ctx: Ctx) {

	suspend fun handle(m: String, p: JSONObject): Any? = when (m) {
		"ping" -> "pong"

		"sources" -> JSONArray(ctx.parsers.sources.map { s ->
			JSONObject().put("name", s.name).put("title", s.title).put("locale", s.locale)
				.put("type", s.contentType.name).put("broken", s.isBroken)
		})

		"info" -> {
			val parser = ctx.parser(p.getString("source"))
			val caps = parser.filterCapabilities
			JSONObject()
				.put("orders", JSONArray(parser.availableSortOrders.map { it.name }))
				.put("search", caps.isSearchSupported)
				.put("domain", parser.domain)
				.put("domains", JSONArray(parser.configKeyDomain.presetValues.toList()))
		}

		"tags" -> {
			val opts = ctx.parser(p.getString("source")).getFilterOptions()
			JSONArray(opts.availableTags.sortedBy { it.title }.map(::tagJson))
		}

		"setDomain" -> { ctx.setDomain(p.getString("source"), p.str("domain")); true }

		"list" -> {
			val source = p.getString("source")
			val parser = ctx.parser(source)
			val order = p.str("order")?.let { runCatching { SortOrder.valueOf(it) }.getOrNull() }
				?.takeIf { it in parser.availableSortOrders }
				?: parser.availableSortOrders.let { if (SortOrder.POPULARITY in it) SortOrder.POPULARITY else it.first() }
			val src = srcOf(ctx, source)
			val tags = p.optJSONArray("tags")?.let { a ->
				(0 until a.length()).map { a.getJSONObject(it) }.map { MangaTag(it.getString("title"), it.getString("key"), src) }.toSet()
			} ?: emptySet()
			val filter = MangaListFilter(query = p.str("query")?.takeIf { it.isNotBlank() }, tags = tags)
			JSONArray(parser.getList(p.optInt("offset", 0), order, filter).map(::mangaJson))
		}

		"details" -> {
			val manga = mangaFrom(ctx, p.getJSONObject("manga"))
			mangaJson(ctx.parser(manga.source.name).getDetails(manga))
		}

		"related" -> {
			val manga = mangaFrom(ctx, p.getJSONObject("manga"))
			JSONArray(ctx.parser(manga.source.name).getRelatedManga(manga).map(::mangaJson))
		}

		"pages" -> {
			val source = p.getString("source")
			val ch = chapterFrom(ctx, source, p.getJSONObject("chapter"))
			JSONArray(ctx.parser(source).getPages(ch).map { pg ->
				JSONObject().put("id", pg.id).put("url", pg.url).put("preview", pg.preview)
			})
		}

		// Download an image (cover or page) into `dest`. For pages, `page` is given and resolved first.
		"fetch" -> {
			val source = p.getString("source")
			val parser = ctx.parser(source)
			val src = srcOf(ctx, source)
			val url = p.optJSONObject("page")?.let { o ->
				parser.getPageUrl(MangaPage(o.getLong("id"), o.getString("url"), o.str("preview"), src))
			} ?: p.getString("url")
			val dest = File(p.getString("dest"))
			val req = Request.Builder().url(url).get()
				.headers(parser.getRequestHeaders())
				.header("Accept", "image/avif,image/webp,image/png,image/jpeg,*/*;q=0.8")
				.tag(MangaSource::class.java, src)
				.build()
			ctx.httpClient.newCall(req).await().use { resp ->
				if (!resp.isSuccessful) error("HTTP ${resp.code} for $url")
				dest.parentFile?.mkdirs()
				val tmp = File(dest.path + ".part")
				resp.body.byteStream().use { input -> tmp.outputStream().use { input.copyTo(it, 64 * 1024) } }
				if (!tmp.renameTo(dest)) error("rename failed")
				JSONObject().put("path", dest.path).put("mime", resp.header("Content-Type"))
			}
		}

		"resolve" -> ctx.resolveLink(p.getString("url"))?.let(::mangaJson)

		// Plain HTTP for the app itself (phone sync), through the same client
		"http" -> {
			val body = p.str("body")?.toRequestBody("application/json".toMediaTypeOrNull())
			val req = Request.Builder().url(p.getString("url"))
				.method(p.optString("method", "GET"), body)
				.header("User-Agent", "Kotatsu-Desktop")
			p.optJSONObject("headers")?.let { h -> h.keys().forEach { k -> req.header(k, h.getString(k)) } }
			ctx.httpClient.newCall(req.build()).await().use { resp ->
				JSONObject().put("code", resp.code).put("body", resp.body.string())
			}
		}

		else -> error("Unknown method $m")
	}
}

fun main(args: Array<String>) {
	System.setProperty("java.awt.headless", "true")
	val dataDir = File(args.firstOrNull() ?: (System.getProperty("user.home") + "/.local/share/kotatsu-desktop"))
	val ctx = Ctx(dataDir)
	val bridge = Bridge(ctx)
	val out = BufferedWriter(OutputStreamWriter(FileOutputStream(FileDescriptor.out), Charsets.UTF_8), 1 shl 16)
	val outLock = Any()
	fun send(o: JSONObject) = synchronized(outLock) { out.write(o.toString()); out.write('\n'.code); out.flush() }

	val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO.limitedParallelism(48))
	val jobs = ConcurrentHashMap<Long, Job>()

	// Persist cookies periodically
	scope.launch { while (true) { delay(15_000); ctx.cookieJar.flush() } }
	Runtime.getRuntime().addShutdownHook(Thread { ctx.cookieJar.flush() })

	val input = BufferedReader(InputStreamReader(System.`in`, Charsets.UTF_8), 1 shl 16)
	while (true) {
		val line = input.readLine() ?: break
		if (line.isBlank()) continue
		val req = runCatching { JSONObject(line) }.getOrNull() ?: continue
		val id = req.optLong("id")
		val m = req.optString("m")
		val p = req.optJSONObject("p") ?: JSONObject()
		if (m == "cancel") { jobs.remove(p.optLong("id"))?.cancel(); continue }
		jobs[id] = scope.launch {
			try {
				val r = bridge.handle(m, p)
				send(JSONObject().put("id", id).put("r", r ?: JSONObject.NULL))
			} catch (e: CancellationException) {
				send(JSONObject().put("id", id).put("e", "cancelled"))
			} catch (e: Throwable) {
				send(JSONObject().put("id", id).put("e", (e::class.simpleName ?: "Error") + ": " + (e.message ?: "")))
			} finally {
				jobs.remove(id)
			}
		}
	}
	ctx.cookieJar.flush()
	scope.cancel()
	// OkHttp's idle threads would otherwise keep the process alive for a minute
	kotlin.system.exitProcess(0)
}
