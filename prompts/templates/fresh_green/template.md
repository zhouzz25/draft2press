松溪清新自然风
主色 #52a479 嫩绿 #8ec371 点缀紫 #a65bcb 底纹绿 rgba(97,137,23,0.17) 正文 #3f3f3f
适合：乡村文旅、自然风光、传统文化、活动纪实类文章

本模板复刻自「计小狸 · 松溪」系列推文：清新自然系配色、大圆角卡片、
四角 L 型细边装饰框、金色/绿色小竖条点缀、图片上方带深色渐化标题帽。
所有样式可直接照搬；{{asset:fresh_green/xxx}} 为模板自带贴图素材。

==== 核心样式 ====

大标题样式（居中、艺术感字距，绿→嫩绿横条双色）：
<p style="text-align:center;font-size:150%;letter-spacing:3px;color:#2f6b52;padding:28px 0 6px 0;margin:0"><strong>标题</strong></p>
<p style="text-align:center;margin:0"><span style="display:inline-block;width:48px;height:4px;background:#8ec371;border-radius:2px;margin-right:4px"></span><span style="display:inline-block;width:14px;height:4px;background:#a65bcb;border-radius:2px"></span></p><br>

小节标题（居中自适应横条，紫绿双色圆角，禁止铺满整行）：
<p style="text-align:center;margin:0"><span style="background:#52a479;color:#fff;display:inline-block;padding:7px 18px;font-size:112.5%;letter-spacing:2px;border-radius:16px;box-shadow:1px 2px 0 #a65bcb"><strong>01 章节名</strong></span></p><br>

正文卡片（巨大圆角 + 浅底纹色 + 左开门色条装饰）：
<p style="background:rgba(97,137,23,0.08);border-radius:15px;padding:20px 22px;font-size:100%;line-height:2;letter-spacing:1px;color:#3f3f3f"><span style="display:inline-block;width:36px;height:4px;background:#8ec371;border-radius:2px;vertical-align:middle;margin:0 0 4px 0"></span>&emsp;&emsp;正文内容</p><br>

四角贴边装饰卡（重点内容/金句专用，四角 L 型细边框，绿+紫双色）：
<p style="position:relative;background:rgba(97,137,23,0.10);border-radius:15px;padding:18px;text-align:center;color:#2f6b52;font-style:italic"><span style="position:absolute;left:10px;top:10px;width:14px;height:14px;border-color:#8ec371;border-style:solid;border-width:2px 0 0 2px;border-radius:2px 0 0 0"></span><span style="position:absolute;right:10px;top:10px;width:14px;height:14px;border-color:#a65bcb;border-style:solid;border-width:2px 2px 0 0;border-radius:0 2px 0 0"></span><span style="position:absolute;left:10px;bottom:10px;width:14px;height:14px;border-color:#a65bcb;border-style:solid;border-width:0 0 2px 2px;border-radius:0 0 0 2px"></span><span style="position:absolute;right:10px;bottom:10px;width:14px;height:14px;border-color:#8ec371;border-style:solid;border-width:0 2px 2px 0;border-radius:0 0 2px 0"></span>「金句或强调内容」「金句第二行」</p><br>

关键词高亮（每卡 1-2 个）：
<strong style="color:#52a479">关键词</strong>
强调进阶（加浅紫底）：<strong style="color:#52a479;background:rgba(166,91,203,0.12);padding:0 3px;border-radius:3px">关键词</strong>

分隔线（双色交叠圆点线）：
<p style="text-align:center"><span style="display:inline-block;width:34%;height:1px;background:linear-gradient(90deg,rgba(82,164,121,0) 0%,#52a479 100%);vertical-align:middle"></span><span style="display:inline-block;width:6px;height:6px;background:#8ec371;border-radius:50%;vertical-align:middle;margin:0 10px"></span><span style="display:inline-block;width:34%;height:1px;background:linear-gradient(270deg,rgba(166,91,203,0) 0%,#a65bcb 100%);vertical-align:middle"></span></p><br>

结尾 END 区（双侧细线夹住绿色徽章）：
<br><p style="text-align:center"><span style="display:inline-block;width:35%;height:1px;background:rgba(97,137,23,0.35);vertical-align:middle"></span><span style="display:inline-block;background:#52a479;color:#fff;display:inline-block;padding:5px 20px;font-size:112.5%;letter-spacing:2px;border-radius:16px;margin:0 10px"><strong>END</strong></span><span style="display:inline-block;width:35%;height:1px;background:rgba(97,137,23,0.35);vertical-align:middle"></span></p><br>


==== 可用贴图素材及使用方法 ====
本模板没有贴图素材，装饰全部纯 CSS 可实现，包括：
- 正文/金句卡四角的四个小啾啾：见「四角贴边装饰卡」，即左上/右上/左下/右下四个 14px 的 L 型细角标（border-width:2px 0 0 2px 等四种组合）
- 小节标题后面垫着的半块标识：药丸本体 + box-shadow:1px 2px 0 #a65bcb 错位投影，视觉上像背后垫了半块紫色小色标

==== 图文混排规则 ====
- 本模板不提供 {{asset:...}} 贴图，严禁虚构
- 正文配图使用 {{photo:文件名}}，图片段落前后各留 <br>
- 每张正文卡片开头用「卡片开门绿色短条」装饰（见样式）
- 与自然/绿色主题相关的关键词套关键词高亮
